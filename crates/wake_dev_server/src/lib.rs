//! wake_dev_server — Dev Server + HMR（DESIGN §7 / PLAN Phase 5）。
//!
//! `wake dev <root>`：以 actix-web 起 HTTP 服务，从**内存**服务增量打包产物；notify 监听源码变更
//! （20ms 防抖）→ `IncrementalBundler` 增量重建 → 经 WebSocket 广播事件 → 浏览器 client runtime
//! 触发 live-reload / 显示错误 overlay / 断连自动重连。SPA fallback：未知路径回退到 HTML。
//!
//! 本切片实现 5.1 文件监听 + 5.4 dev server + 5.5 HMR 协议（live-reload 兜底形态）。
//! React Fast Refresh（5.6 状态保留热更）、依赖预扫描（5.2）、摘要防火墙任务（5.3）、
//! 热路径预算断言（5.7）为后续切片。
//!
//! 线程模型：**监听线程独占 `IncrementalBundler`**（在该线程创建 + 重建），只把产物 `String`
//! 经 `RwLock` 跨线程共享 → 无需 bundler 满足 `Send`。HTTP 处理器只读共享产物。

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use futures_util::StreamExt as _;
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use wake_bundler::{BuildRequest, BuildSession, IncrementalBundler, ResolveOptions};
use wake_common::{Diagnostic, OsFileSystem};
use wake_ecma_transform::TargetEnv;

// —— 终端着色（tty + 非 NO_COLOR 时启用）——
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy)]
struct Sty {
    color: bool,
    quiet: bool,
}
impl Sty {
    fn detect(quiet: bool) -> Sty {
        Sty {
            color: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
            quiet,
        }
    }
    fn p(&self, code: &str, s: &str) -> String {
        if self.color {
            format!("{code}{s}{RESET}")
        } else {
            s.to_string()
        }
    }
    fn brand(&self, s: &str) -> String {
        self.p("\x1b[1;38;5;213m", s)
    }
    fn ok(&self, s: &str) -> String {
        self.p("\x1b[1;38;5;114m", s)
    }
    fn err(&self, s: &str) -> String {
        self.p("\x1b[31m", s)
    }
    fn dim(&self, s: &str) -> String {
        self.p("\x1b[2m", s)
    }
    fn accent(&self, s: &str) -> String {
        self.p("\x1b[38;5;81m", s)
    }
    fn bold(&self, s: &str) -> String {
        self.p("\x1b[1m", s)
    }
    fn warn(&self, s: &str) -> String {
        self.p("\x1b[33m", s)
    }
}

fn human_dur(d: Duration) -> String {
    let ms = d.as_secs_f64() * 1000.0;
    if ms < 1000.0 {
        format!("{:.0} ms", ms.max(1.0))
    } else {
        format!("{:.2} s", ms / 1000.0)
    }
}

struct BuildSummary {
    modules: usize,
    chunks: usize,
    assets: usize,
    duration: String,
}

/// 当前产物状态（跨线程共享）。
struct BundleState {
    /// 最近一次成功构建的**入口** chunk（服务于 `/bundle.js`）。
    js: String,
    /// 非入口 chunk：`文件名 → 源码`。代码分割后由运行时以
    /// `<script src=publicPath+file>` 拉取，dev 必须能按文件名提供。
    chunks: std::collections::HashMap<String, String>,
    /// 带外资源产物：`文件名 → 字节`（超阈值的图片/字体等）。
    assets: std::collections::HashMap<String, Vec<u8>>,
    /// 最近一次构建的 Source Map V3 JSON（`None` = 未产出）。WAKE-COMPATIBILITY §M4d。
    map: Option<String>,
    /// 若最近一次构建失败，格式化后的诊断文本；否则 `None`。
    error: Option<String>,
}

/// HTTP 处理器共享数据。
struct AppState {
    bundle: Arc<RwLock<BundleState>>,
    /// HMR 事件广播（消息本身为 JSON 文本）。
    tx: broadcast::Sender<String>,
    /// 注入了 HMR client 脚本的 HTML 外壳。
    html: Arc<RwLock<String>>,
    /// 代理规则（已编译）；命中前缀的请求转发到后端 target。
    proxies: Arc<Vec<CompiledProxy>>,
    /// `public/` 静态资源目录（保持既定行为 / Vite：原样映射到 URL 根）。
    public_dir: PathBuf,
}

/// 文件变化后、BuildSession 失效前运行的生成钩子。返回需要一并失效的生成文件。
pub type BeforeRebuild =
    Arc<dyn Fn(&[PathBuf]) -> Result<Vec<PathBuf>, String> + Send + Sync + 'static>;
#[derive(Debug, Clone)]
pub enum ServerEvent {
    RebuildStart { changed_paths: Vec<PathBuf> },
    Rebuilt { modules: usize, duration_ms: f64 },
    Diagnostic { message: String },
    Closed,
}

pub type EventHandler = Arc<dyn Fn(ServerEvent) + Send + Sync + 'static>;

/// Dev server 选项（由 CLI 读 `wake.config.toml` 装配）。WAKE-COMPATIBILITY §M3。
pub struct ServeOptions {
    /// 已由调用方解析完成的入口文件。
    pub entry: PathBuf,
    /// 解析选项（含别名 `@`/`@@`/`@@@`）。
    pub resolve_options: ResolveOptions,
    /// 编译期 define（dev 口径：`process.env.NODE_ENV → "development"` + 用户 `[define]`）。
    pub define: Vec<(String, String)>,
    /// 监听地址（缺省 `127.0.0.1`；设 `0.0.0.0` 可局域网访问）。
    pub host: String,
    /// 启动后自动打开浏览器。
    pub open: bool,
    /// 代理规则（转发匹配前缀的请求到后端 target，保持既定行为 `devServer.proxy`）。
    pub proxy: Vec<ProxyRule>,
    /// 已由配置层解析并规范化的浏览器目标。
    pub target_env: TargetEnv,
    /// React automatic runtime 包名（`react`、`preact` 等）。
    pub jsx_import_source: String,
    /// 额外监听根目录；为空时保持普通应用的 `src/` 默认行为。
    pub watch_roots: Vec<PathBuf>,
    /// 文档/扫描模块等生成步骤，在 BuildSession 失效之前执行。
    pub before_rebuild: Option<BeforeRebuild>,
    /// Suppress terminal presentation; library frontends should enable this.
    pub quiet: bool,
    /// Optional structured event sink used by library frontends.
    pub event_handler: Option<EventHandler>,
}

impl Default for ServeOptions {
    fn default() -> ServeOptions {
        ServeOptions {
            entry: PathBuf::from("src/index.tsx"),
            resolve_options: ResolveOptions::default(),
            define: Vec::new(),
            host: "127.0.0.1".to_string(),
            open: false,
            proxy: Vec::new(),
            target_env: TargetEnv::default(),
            jsx_import_source: "react".to_string(),
            watch_roots: Vec::new(),
            before_rebuild: None,
            quiet: false,
            event_handler: None,
        }
    }
}

/// 一条代理规则（保持既定行为 `Proxy`）。
#[derive(Clone)]
pub struct ProxyRule {
    /// 匹配的路径前缀（如 `["/api"]`）。
    pub context: Vec<String>,
    /// 转发目标（如 `http://localhost:8080`）。
    pub target: String,
    /// 路径改写：`(正则, 替换)`（如 `("^/api", "")`）。按序应用。
    pub path_rewrite: Vec<(String, String)>,
    /// 是否把请求头 `Host` 改写为 target 的 host（跨域远端需开）。
    pub change_origin: bool,
}

/// 启动 dev server（阻塞直到进程退出）。`root` 为项目根，`port` 为监听端口，`options` 见 [`ServeOptions`]。
pub fn serve(root: &Path, port: u16, options: ServeOptions) -> std::io::Result<()> {
    start(root, port, options)?.wait()
}

fn run_server(
    root: &Path,
    port: u16,
    options: ServeOptions,
    started_tx: mpsc::Sender<Result<StartedServer, String>>,
    stop: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let ServeOptions {
        entry,
        resolve_options,
        define,
        host,
        open,
        proxy,
        target_env,
        jsx_import_source,
        watch_roots,
        before_rebuild,
        quiet,
        event_handler,
    } = options;
    // 编译代理规则（pathRewrite 正则一次编译）。非法正则跳过并告警。
    let proxies: Vec<CompiledProxy> = proxy
        .into_iter()
        .filter_map(CompiledProxy::compile)
        .collect();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let entry = if entry.is_absolute() {
        entry
    } else {
        root.join(entry)
    };
    if !entry.is_file() {
        return Err(std::io::Error::other(format!(
            "入口文件不存在：{}",
            entry.display()
        )));
    }
    let html = Arc::new(RwLock::new(load_html_template(&root)));

    let sty = Sty::detect(quiet);
    let bundle = Arc::new(RwLock::new(BundleState {
        js: String::new(),
        chunks: std::collections::HashMap::new(),
        assets: std::collections::HashMap::new(),
        map: None,
        error: None,
    }));
    let (tx, _rx) = broadcast::channel::<String>(64);

    // 品牌行保持克制；运行状态与构建数据在首次构建结束后统一展示。
    if !sty.quiet {
        println!();
        println!(
            "  {}  {} {} {}  {}",
            sty.warn("⚡"),
            sty.brand("wake"),
            sty.dim("/"),
            sty.bold("dev"),
            sty.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
        );
    }

    // —— 监听线程：独占 bundler，负责首次构建 + 增量重建 + 广播 ——
    let (ready_tx, ready_rx) = mpsc::channel::<Option<BuildSummary>>();
    let watcher_stop = Arc::clone(&stop);
    let watcher_join = {
        let bundle = bundle.clone();
        let tx = tx.clone();
        let html = html.clone();
        let entry = entry.clone();
        let watch_root = root.clone();
        let watcher_events = event_handler.clone();
        std::thread::Builder::new()
            .name("wake-dev-watch".into())
            .spawn(move || {
                watch_and_rebuild(
                    watch_root,
                    entry,
                    bundle,
                    html,
                    tx,
                    ready_tx,
                    sty,
                    resolve_options,
                    define,
                    target_env,
                    jsx_import_source,
                    watch_roots,
                    before_rebuild,
                    watcher_stop,
                    watcher_events,
                );
            })
            .expect("spawn watcher thread")
    };
    // 等首次构建完成再开始服务（保证第一屏有产物）。
    let summary = ready_rx.recv().ok().flatten();

    // 浏览器展示地址：0.0.0.0 时用 localhost。
    let display_host = if host == "0.0.0.0" {
        "localhost"
    } else {
        host.as_str()
    };
    let url = format!("http://{display_host}:{port}/");

    if !sty.quiet {
        if let Some(summary) = &summary {
            println!();
            println!(
                "  {}  {}",
                sty.ok("●"),
                sty.bold(&format!("Ready in {}", summary.duration))
            );
        }

        println!();
        println!("     {}  {}", sty.dim("Local"), sty.accent(&url));

        if let Some(summary) = summary {
            println!();
            println!(
                "     {}   {}   {}",
                sty.accent(&format!("{} modules", summary.modules)),
                sty.dim(&format!("{} chunks", summary.chunks)),
                sty.dim(&format!("{} assets", summary.assets))
            );
            println!(
                "     {}",
                sty.dim("HMR on  ·  source maps on  ·  watching for changes")
            );
        }

        if !proxies.is_empty() {
            println!();
            for p in &proxies {
                println!(
                    "     {}  {} {} {}",
                    sty.dim("Proxy"),
                    sty.dim(&p.context.join(",")),
                    sty.accent("→"),
                    sty.accent(&p.target)
                );
            }
        }
        println!();
        println!("     {}", sty.dim("Press Ctrl+C to stop"));
        println!();
    }

    // 自动打开浏览器（启动后）。
    if open {
        open_browser(&url);
    }

    let data = web::Data::new(AppState {
        bundle,
        tx,
        html,
        proxies: Arc::new(proxies),
        public_dir: root.join("public"),
    });
    let server = HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            // 放宽负载上限，便于代理转发较大的 POST 请求体。
            .app_data(web::PayloadConfig::new(64 * 1024 * 1024))
            .route("/bundle.js", web::get().to(serve_bundle))
            .route("/bundle.js.map", web::get().to(serve_bundle_map))
            .route("/__wake/client.js", web::get().to(serve_client))
            .route("/__wake_hmr", web::get().to(ws_handler))
            // 默认服务：先试代理转发（任意方法），未命中且为 GET 则回退 SPA HTML。
            .default_service(web::to(serve_default))
    })
    .bind((host.as_str(), port));
    let server = match server {
        Ok(server) => server.workers(2).run(),
        Err(error) => {
            stop.store(true, Ordering::Release);
            let _ = watcher_join.join();
            return Err(error);
        }
    };
    let handle = server.handle();
    if started_tx
        .send(Ok(StartedServer {
            url: url.clone(),
            handle: handle.clone(),
        }))
        .is_err()
    {
        stop.store(true, Ordering::Release);
        actix_web::rt::System::new().block_on(handle.stop(false));
        let _ = watcher_join.join();
        return Err(std::io::Error::other(
            "Wake dev server startup receiver was dropped",
        ));
    }
    let result = actix_web::rt::System::new().block_on(server);
    stop.store(true, Ordering::Release);
    let _ = watcher_join.join();
    if let Some(handler) = event_handler {
        handler(ServerEvent::Closed);
    }
    result
}

/// 已编译的代理规则（pathRewrite 正则预编译）。
struct CompiledProxy {
    context: Vec<String>,
    target: String,
    rewrites: Vec<(regex::Regex, String)>,
    change_origin: bool,
}

impl CompiledProxy {
    fn compile(p: ProxyRule) -> Option<CompiledProxy> {
        let mut rewrites = Vec::new();
        for (pat, rep) in p.path_rewrite {
            match regex::Regex::new(&pat) {
                Ok(re) => rewrites.push((re, rep)),
                Err(e) => {
                    eprintln!("  代理 pathRewrite 正则非法 `{pat}`：{e}（跳过该改写）");
                }
            }
        }
        Some(CompiledProxy {
            context: p.context,
            target: p.target,
            rewrites,
            change_origin: p.change_origin,
        })
    }

    /// 路径是否命中本规则的任一 context 前缀。
    fn matches(&self, path: &str) -> bool {
        self.context.iter().any(|c| path.starts_with(c.as_str()))
    }

    /// 应用 pathRewrite（按序正则替换）。
    fn rewrite(&self, path: &str) -> String {
        let mut p = path.to_string();
        for (re, rep) in &self.rewrites {
            p = re.replace(&p, rep.as_str()).into_owned();
        }
        p
    }
}

/// 跨平台打开浏览器（尽力而为，失败静默）。
fn open_browser(url: &str) {
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

// ======================================================================
// 监听 + 重建
// ======================================================================

fn watch_and_rebuild(
    root: PathBuf,
    entry: PathBuf,
    bundle: Arc<RwLock<BundleState>>,
    html: Arc<RwLock<String>>,
    tx: broadcast::Sender<String>,
    ready_tx: mpsc::Sender<Option<BuildSummary>>,
    sty: Sty,
    resolve_options: ResolveOptions,
    define: Vec<(String, String)>,
    target_env: TargetEnv,
    jsx_import_source: String,
    watch_roots: Vec<PathBuf>,
    before_rebuild: Option<BeforeRebuild>,
    stop: Arc<AtomicBool>,
    event_handler: Option<EventHandler>,
) {
    let mut bundler = IncrementalBundler::new(Arc::new(OsFileSystem));
    // 别名（@/@@）+ define（dev 口径）须在首次 build 前设置，dev 与 build 一致。
    bundler.set_resolve_options(resolve_options);
    bundler.set_define(define);
    bundler.set_target_env(target_env);
    // dev 走非 minify 单包路径 → 可产出精确 sourcemap（WAKE-COMPATIBILITY §M4d）。
    bundler.enable_sourcemap();
    // 零运行时 CSS-in-JS（§M5）：dev 不抽取 `.css`，抽出的样式随模块体 `<style>` 注入，
    // 与 `.css` 模块的 dev 行为一致。项目未用 Linaria 时零开销。
    bundler.enable_css_in_js();
    // 代码分割：与 prod 行为一致——动态 `import()` 切出 async chunk。此前 dev 不开分割，
    // 懒加载模块被内联进单包（能跑但不懒加载），与生产产物结构不一致、掩盖分割相关问题。
    bundler.enable_code_splitting();
    // JSX **dev runtime**：`jsxDEV` 携带 `{fileName,lineNumber,columnNumber}`，
    // React DevTools 借此显示组件栈、报错能定位到源文件行列（保持既定行为 的 dev 口径）。
    // 该口径已混入 `content_key`，与 prod 的模块摘要缓存互不干扰。
    bundler.set_jsx_runtime(true, Box::leak(jsx_import_source.into_boxed_str()));
    let mut session = BuildSession::from_incremental(bundler);
    // 首次构建。
    let summary = rebuild(
        &mut session,
        &entry,
        &bundle,
        &tx,
        true,
        sty,
        event_handler.as_ref(),
    );
    let _ = ready_tx.send(summary);

    // 普通应用默认监听 src；文档模式可提供 docs/src 等多个根目录。
    let default_watch_dir = {
        let src = root.join("src");
        if src.is_dir() { src } else { root.clone() }
    };
    let mut watch_targets: Vec<(PathBuf, RecursiveMode)> = if watch_roots.is_empty() {
        vec![(default_watch_dir.clone(), RecursiveMode::Recursive)]
    } else {
        watch_roots
            .into_iter()
            .filter_map(|path| {
                let path = if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                };
                if path.is_dir() {
                    Some((path, RecursiveMode::Recursive))
                } else {
                    path.parent()
                        .map(|parent| (parent.to_path_buf(), RecursiveMode::NonRecursive))
                }
            })
            .collect()
    };
    let (evt_tx, evt_rx) = mpsc::channel::<(Vec<PathBuf>, bool)>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && is_source_event(&ev)
            {
                let structural = is_structural_event(&ev);
                let _ = evt_tx.send((ev.paths, structural));
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("  {} 无法创建文件监听器：{e}", sty.err("✗"));
                return;
            }
        };
    let public_dir = root.join("public");
    if public_dir.is_dir() {
        watch_targets.push((public_dir, RecursiveMode::Recursive));
    }
    watch_targets.push((root.clone(), RecursiveMode::NonRecursive));
    watch_targets.sort_by(|left, right| left.0.cmp(&right.0));
    watch_targets.dedup_by(|left, right| left.0 == right.0);
    for (watch_dir, mode) in watch_targets {
        if let Err(error) = watcher.watch(&watch_dir, mode) {
            eprintln!(
                "  {} 无法监听 {}：{error}",
                sty.err("✗"),
                watch_dir.display()
            );
            return;
        }
    }
    while !stop.load(Ordering::Acquire) {
        let (mut changed, mut structural) = match evt_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        // 落盘沉降：给 OS 少许时间完成写入（避免读到未 flush 的旧内容），
        // 再排空同批事件直到 20ms 静默（防抖）。
        std::thread::sleep(Duration::from_millis(30));
        while let Ok((paths, event_structural)) = evt_rx.recv_timeout(Duration::from_millis(20)) {
            changed.extend(paths);
            structural |= event_structural;
        }
        changed.sort();
        changed.dedup();
        if let Some(handler) = &event_handler {
            handler(ServerEvent::RebuildStart {
                changed_paths: changed.clone(),
            });
        }
        if let Some(regenerate) = &before_rebuild {
            match regenerate(&changed) {
                Ok(mut generated) => {
                    structural |= !generated.is_empty();
                    changed.append(&mut generated);
                    changed.sort();
                    changed.dedup();
                }
                Err(error) => {
                    {
                        let mut state = bundle.write().unwrap();
                        state.error = Some(error.clone());
                    }
                    if !sty.quiet {
                        eprintln!("  {} 生成步骤失败：{error}", sty.err("✗"));
                    }
                    if let Some(handler) = &event_handler {
                        handler(ServerEvent::Diagnostic {
                            message: error.clone(),
                        });
                    }
                    let _ = tx.send(msg_error(&error));
                    continue;
                }
            }
        }
        // HTML 外壳不经过 bundler，必须在通知浏览器刷新前单独刷新共享模板。
        *html.write().unwrap() = load_html_template(&root);
        session.invalidate_paths(&changed, structural);
        let _ = rebuild(
            &mut session,
            &entry,
            &bundle,
            &tx,
            false,
            sty,
            event_handler.as_ref(),
        );
    }
}

/// notify 事件是否为源码相关（忽略目录/元数据类噪声）。
fn is_source_event(ev: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && ev.paths.iter().any(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(is_watched_ext)
    })
}

fn is_structural_event(ev: &notify::Event) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

/// 触发重建的扩展名。
///
/// 图片与字体必须在内：它们既可能被 JS `import`，也可能被 CSS 的 `url()` 引用，两条路径
/// 都会把字节内容（dev 下是 base64 内联）打进产物——换一张图不重建，页面就还是旧的。
fn is_watched_ext(e: &str) -> bool {
    matches!(
        e,
        "ts" | "tsx"
            | "md"
            | "mdx"
            | "js"
            | "jsx"
            | "mts"
            | "cts"
            | "json"
            | "html"
            | "css"
            | "raw"
            // 图片
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "avif"
            | "ico"
            | "bmp"
            // 字体
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
    )
}

/// 执行一次（增量）构建并更新共享状态 + 广播 HMR 事件。
fn rebuild(
    session: &mut BuildSession,
    entry: &Path,
    bundle: &Arc<RwLock<BundleState>>,
    tx: &broadcast::Sender<String>,
    first: bool,
    sty: Sty,
    event_handler: Option<&EventHandler>,
) -> Option<BuildSummary> {
    let t = Instant::now();
    let out = session.build_current_ref(BuildRequest::new(entry));
    let elapsed = t.elapsed();
    let dur = human_dur(elapsed);
    let sep = sty.dim("·");
    if out.has_errors() {
        let errs = out.diagnostics.iter().filter(|d| d.is_error()).count();
        let err = format_diagnostics(&out.diagnostics);
        {
            let mut s = bundle.write().unwrap();
            s.error = Some(err.clone());
        }
        if !sty.quiet {
            eprintln!(
                "  {}  {}  {sep}  {}",
                sty.err("✗"),
                sty.bold("构建失败"),
                sty.err(&format!("{errs} 个错误"))
            );
            for line in err.lines() {
                eprintln!("    {}", sty.dim(line));
            }
        }
        if let Some(handler) = event_handler {
            handler(ServerEvent::Diagnostic {
                message: err.clone(),
            });
        }
        let _ = tx.send(msg_error(&err));
        None
    } else {
        let summary = BuildSummary {
            modules: out.module_count,
            chunks: out.chunks.len(),
            assets: out.assets.len(),
            duration: dur.clone(),
        };
        {
            let mut s = bundle.write().unwrap();
            // 追加 sourceMappingURL 让 DevTools 自动拉取（外链 .map，不膨胀 bundle 体积）。
            let map = out.chunks[out.entry_chunk].source_map.clone();
            s.js = if map.is_some() {
                format!("{}\n//# sourceMappingURL=/bundle.js.map\n", out.bundle)
            } else {
                out.bundle.clone()
            };
            // 非入口 chunk 按**文件名**登记：运行时以 `<script src=publicPath+file>` 拉取，
            // dev 必须能按同名提供。此前未登记 → 请求落到 SPA fallback 拿到 HTML，
            // 浏览器把 HTML 当 JS 执行报语法错误。
            s.chunks = out
                .chunks
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != out.entry_chunk)
                .map(|(_, c)| (c.file_name.clone(), c.code.clone()))
                .collect();
            // 带外资源产物（超阈值图片/字体等）同理按文件名提供。
            s.assets = out
                .assets
                .iter()
                .map(|a| (a.file_name.clone(), a.bytes.clone()))
                .collect();
            s.map = map;
            s.error = None;
        }
        if !first && !sty.quiet {
            eprintln!(
                "  {}  {}  {sep}  {}  {sep}  {}",
                sty.ok("✓"),
                sty.bold("已更新"),
                sty.accent(&format!("{} 模块", out.module_count)),
                sty.dim(&format!("耗时 {dur}")),
            );
        }
        if !first {
            let _ = tx.send(r#"{"type":"reload"}"#.to_string());
            if let Some(handler) = event_handler {
                handler(ServerEvent::Rebuilt {
                    modules: out.module_count,
                    duration_ms: elapsed.as_secs_f64() * 1000.0,
                });
            }
        }
        Some(summary)
    }
}

fn format_diagnostics(diags: &[Diagnostic]) -> String {
    let mut out = String::new();
    for d in diags.iter().filter(|d| d.is_error()) {
        let code = d.code.as_deref().unwrap_or("");
        out.push_str(&format!("[{code}] {}\n", d.message));
    }
    out
}

// ======================================================================
// HTTP 处理器
// ======================================================================

async fn serve_bundle(data: web::Data<AppState>) -> HttpResponse {
    let js = data.bundle.read().unwrap().js.clone();
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .insert_header(("Cache-Control", "no-cache"))
        .body(js)
}

/// 提供 `/bundle.js.map`（DevTools 依 `sourceMappingURL` 自动拉取）。
async fn serve_bundle_map(data: web::Data<AppState>) -> HttpResponse {
    match data.bundle.read().unwrap().map.clone() {
        Some(map) => HttpResponse::Ok()
            .content_type("application/json; charset=utf-8")
            .insert_header(("Cache-Control", "no-cache"))
            .body(map),
        None => HttpResponse::NotFound().body("no source map"),
    }
}

async fn serve_client() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .body(CLIENT_RUNTIME)
}

/// 服务 HTML（含 SPA fallback：任何未知 GET 路径都回退到应用外壳）。
async fn serve_html(data: web::Data<AppState>) -> HttpResponse {
    let html = data.html.read().unwrap().clone();
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "no-cache"))
        .body(html)
}

/// 默认服务，按序尝试：
/// ① 代理前缀（任意方法）→ 转发后端；
/// ② 分割产生的 async/shared **chunk**（按文件名）；
/// ③ 带外**资源产物**（超阈值图片/字体等）；
/// ④ **`public/` 静态文件**（保持既定行为 / Vite，原样映射到 URL 根）；
/// ⑤ SPA 回退 —— **仅当路径不像文件时**。
///
/// ⑤ 的限定是关键：此前任何未知 GET 都返回 HTML，于是 `/logo.png`、`/a.chunk.js` 一律拿到
/// 200 + HTML —— 浏览器把 HTML 当 JS 执行报语法错误、当图片渲染则空白，且**看不出是 404**。
/// 现在带扩展名的路径未命中即 404（对齐 webpack-dev-server 的 `disableDotRule: false`）。
async fn serve_default(
    req: HttpRequest,
    body: web::Bytes,
    data: web::Data<AppState>,
) -> HttpResponse {
    if let Some(i) = data.proxies.iter().position(|p| p.matches(req.path())) {
        return forward(&req, body, &data.proxies[i]).await;
    }
    if req.method() != actix_web::http::Method::GET {
        return HttpResponse::NotFound().finish();
    }

    let rel = req.path().trim_start_matches('/');

    // ② chunk（内存）
    if let Some(code) = data.bundle.read().unwrap().chunks.get(rel).cloned() {
        return HttpResponse::Ok()
            .content_type("application/javascript; charset=utf-8")
            .insert_header(("Cache-Control", "no-cache"))
            .body(code);
    }
    // ③ 资源产物（内存）
    if let Some(bytes) = data.bundle.read().unwrap().assets.get(rel).cloned() {
        return HttpResponse::Ok()
            .content_type(mime_for(rel))
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes);
    }
    // ④ public/ 静态文件
    if let Some((bytes, ct)) = read_public_file(&data.public_dir, rel) {
        return HttpResponse::Ok()
            .content_type(ct)
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes);
    }
    // ⑤ SPA 回退：仅无扩展名的路径（前端路由），形似文件者 404。
    if looks_like_file(rel) {
        return HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body(format!("wake dev: 未找到 `/{rel}`"));
    }
    serve_html(data).await
}

/// 路径末段是否含扩展名（`assets/a.png` → true；`users/1` → false）。
fn looks_like_file(rel: &str) -> bool {
    rel.rsplit('/')
        .next()
        .is_some_and(|last| last.contains('.'))
}

/// 从 `public/` 读取静态文件；返回 `(字节, content-type)`。
///
/// **防目录穿越**：规范化后必须仍在 `public_dir` 之内，否则拒绝——`/../../etc/passwd`
/// 这类请求不得逃出该目录。
fn read_public_file(public_dir: &Path, rel: &str) -> Option<(Vec<u8>, &'static str)> {
    if rel.is_empty() {
        return None;
    }
    let candidate = public_dir.join(rel);
    let real = candidate.canonicalize().ok()?;
    let base = public_dir.canonicalize().ok()?;
    if !real.starts_with(&base) || !real.is_file() {
        return None;
    }
    let bytes = std::fs::read(&real).ok()?;
    Some((bytes, mime_for(rel)))
}

/// 按扩展名给 content-type（仅覆盖 dev 常见类型，未知走 octet-stream）。
fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "js" | "mjs" | "cjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" | "htm" => "text/html; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "eot" => "application/vnd.ms-fontobject",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        _ => "application/octet-stream",
    }
}

/// 把请求转发到代理 target（buffer 整个 body；dev 用，非流式）。
async fn forward(req: &HttpRequest, body: web::Bytes, p: &CompiledProxy) -> HttpResponse {
    use actix_web::http::header;

    let new_path = p.rewrite(req.path());
    let qs = req.query_string();
    let base = p.target.trim_end_matches('/');
    let url = if qs.is_empty() {
        format!("{base}{new_path}")
    } else {
        format!("{base}{new_path}?{qs}")
    };

    let client = awc::Client::default();
    // no_decompress：保持上游压缩体与 Content-Encoding 头一致（不解压后再原样转发头）。
    let mut fwd = client.request(req.method().clone(), &url).no_decompress();
    for (name, value) in req.headers() {
        // 跳过 Host（按 change_origin 决定）、hop-by-hop 与由 body 重算的 Content-Length。
        if name == header::HOST || name == header::CONNECTION || name == header::CONTENT_LENGTH {
            continue;
        }
        fwd = fwd.insert_header((name.clone(), value.clone()));
    }
    // change_origin=false → 保留原始 Host；true → 不设，awc 从 target URL 自动填 target host。
    if !p.change_origin
        && let Some(h) = req.headers().get(header::HOST)
    {
        fwd = fwd.insert_header((header::HOST, h.clone()));
    }

    match fwd.send_body(body).await {
        Ok(mut resp) => {
            let mut builder = HttpResponse::build(resp.status());
            for (name, value) in resp.headers() {
                if name == header::CONNECTION
                    || name == header::TRANSFER_ENCODING
                    || name == header::CONTENT_LENGTH
                {
                    continue;
                }
                builder.insert_header((name.clone(), value.clone()));
            }
            match resp.body().limit(64 * 1024 * 1024).await {
                Ok(bytes) => builder.body(bytes),
                Err(e) => {
                    HttpResponse::BadGateway().body(format!("wake proxy: 读取上游响应失败：{e}"))
                }
            }
        }
        Err(e) => {
            HttpResponse::BadGateway().body(format!("wake proxy: 转发到 {} 失败：{e}", p.target))
        }
    }
}

/// WebSocket：客户端连接后先推当前状态（错误则显示 overlay），随后转发广播事件。
async fn ws_handler(
    req: HttpRequest,
    body: web::Payload,
    data: web::Data<AppState>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, mut stream) = actix_ws::handle(&req, body)?;
    let mut rx = data.tx.subscribe();
    let init = data.bundle.read().unwrap().error.clone();

    actix_web::rt::spawn(async move {
        // 连接即同步当前状态。
        let first = match init {
            Some(err) => msg_error(&err),
            None => r#"{"type":"ok"}"#.to_string(),
        };
        if session.text(first).await.is_err() {
            return;
        }
        loop {
            tokio::select! {
                biased;
                incoming = stream.next() => match incoming {
                    Some(Ok(actix_ws::Message::Ping(p))) => { let _ = session.pong(&p).await; }
                    Some(Ok(actix_ws::Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    _ => {}
                },
                broadcasted = rx.recv() => match broadcasted {
                    Ok(m) => { if session.text(m).await.is_err() { break; } }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        let _ = session.close(None).await;
    });

    Ok(response)
}

// ======================================================================
// 入口 / HTML / 消息
// ======================================================================

/// 加载 HTML 外壳：优先项目 `public/index.html` / `index.html`，注入 HMR client 脚本；
/// 无则生成默认外壳。
fn load_html_template(root: &Path) -> String {
    let candidates = [root.join("public/index.html"), root.join("index.html")];
    for c in candidates {
        if let Ok(mut html) = std::fs::read_to_string(&c) {
            let inject = "<script src=\"/__wake/client.js\"></script>";
            if let Some(pos) = html.find("</head>") {
                html.insert_str(pos, inject);
            } else {
                html.insert_str(0, inject);
            }
            // 保证有 bundle 脚本引用。
            if !html.contains("bundle.js")
                && let Some(pos) = html.find("</body>")
            {
                html.insert_str(pos, "<script src=\"/bundle.js\"></script>");
            }
            return html;
        }
    }
    default_html()
}

fn default_html() -> String {
    "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"/>\
     <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
     <title>wake dev</title>\
     <script src=\"/__wake/client.js\"></script></head>\
     <body><div id=\"root\"></div><script src=\"/bundle.js\"></script></body></html>"
        .to_string()
}

/// 构造错误消息 JSON（转义 message）。
fn msg_error(err: &str) -> String {
    format!(r#"{{"type":"error","message":"{}"}}"#, json_escape(err))
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// HMR 浏览器端运行时：连接 WS，处理 reload / error overlay / 断连重连。
const CLIENT_RUNTIME: &str = r#"(function () {
  var overlay;
  function ensureOverlay() {
    if (!overlay) {
      overlay = document.createElement("div");
      overlay.id = "__wake_overlay";
      overlay.style.cssText =
        "position:fixed;inset:0;background:rgba(20,0,0,.93);color:#ffd9d9;" +
        "font:13px/1.6 ui-monospace,Menlo,Consolas,monospace;padding:28px;" +
        "white-space:pre-wrap;overflow:auto;z-index:2147483647";
      document.body.appendChild(overlay);
    }
    return overlay;
  }
  function showError(msg) {
    var o = ensureOverlay();
    o.textContent = "⚠ wake 构建错误\n\n" + msg;
    o.style.display = "block";
  }
  function clearError() { if (overlay) overlay.style.display = "none"; }
  function connect() {
    var proto = location.protocol === "https:" ? "wss" : "ws";
    var ws = new WebSocket(proto + "://" + location.host + "/__wake_hmr");
    ws.onmessage = function (e) {
      var m;
      try { m = JSON.parse(e.data); } catch (_) { return; }
      if (m.type === "reload") { clearError(); location.reload(); }
      else if (m.type === "error") { showError(m.message); }
      else if (m.type === "ok") { clearError(); }
    };
    ws.onclose = function () { setTimeout(connect, 1000); };
    ws.onerror = function () { try { ws.close(); } catch (_) {} };
  }
  connect();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn msg_error_is_valid_shape() {
        let m = msg_error("boom \"x\"\nline2");
        assert!(m.starts_with(r#"{"type":"error","message":""#));
        assert!(m.ends_with(r#""}"#));
        assert!(m.contains("\\\"x\\\""));
    }

    #[test]
    fn default_html_has_hooks() {
        let h = default_html();
        assert!(h.contains("/__wake/client.js"));
        assert!(h.contains("/bundle.js"));
        assert!(h.contains("id=\"root\""));
    }

    #[test]
    fn html_changes_are_watched() {
        assert!(is_watched_ext("html"));
    }

    #[test]
    fn proxy_matches_and_rewrites() {
        let p = CompiledProxy::compile(ProxyRule {
            context: vec!["/api".to_string()],
            target: "http://localhost:8080".to_string(),
            path_rewrite: vec![("^/api".to_string(), "".to_string())],
            change_origin: true,
        })
        .unwrap();
        assert!(p.matches("/api/users"));
        assert!(p.matches("/api"));
        assert!(!p.matches("/static/app.js"));
        // pathRewrite 去掉 /api 前缀。
        assert_eq!(p.rewrite("/api/users"), "/users");
        assert_eq!(p.rewrite("/other"), "/other");
    }

    #[test]
    fn proxy_multi_context() {
        let p = CompiledProxy::compile(ProxyRule {
            context: vec!["/api".to_string(), "/auth".to_string()],
            target: "http://localhost:9000".to_string(),
            path_rewrite: vec![],
            change_origin: false,
        })
        .unwrap();
        assert!(p.matches("/api/x"));
        assert!(p.matches("/auth/login"));
        assert!(!p.matches("/assets/x"));
        // 无 rewrite → 原样。
        assert_eq!(p.rewrite("/api/x"), "/api/x");
    }
}

#[cfg(test)]
mod static_serving_tests {
    use super::*;

    #[test]
    fn spa_fallback_only_for_extensionless_paths() {
        // 带扩展名 → 视为文件请求，未命中应 404（而非返回 HTML）
        assert!(looks_like_file("a.page.1234.js"));
        assert!(looks_like_file("assets/logo.png"));
        assert!(looks_like_file("styles.css"));
        // 前端路由 → 回退 SPA
        assert!(!looks_like_file("users/1"));
        assert!(!looks_like_file("about"));
        assert!(!looks_like_file(""));
        // 目录形式的路径也按路由处理
        assert!(!looks_like_file("docs/getting-started"));
    }

    #[test]
    fn mime_covers_dev_common_types() {
        assert!(mime_for("a.js").contains("javascript"));
        assert!(mime_for("a.mjs").contains("javascript"));
        assert_eq!(mime_for("a.css"), "text/css; charset=utf-8");
        assert_eq!(mime_for("a.png"), "image/png");
        assert_eq!(mime_for("a.svg"), "image/svg+xml");
        assert_eq!(mime_for("a.woff2"), "font/woff2");
        assert!(mime_for("a.json").contains("json"));
        // 未知扩展名不猜测
        assert_eq!(mime_for("a.xyz"), "application/octet-stream");
    }

    #[test]
    fn public_file_is_served_and_traversal_is_blocked() {
        let dir = std::env::temp_dir().join("wake_dev_public_test");
        let pubdir = dir.join("public");
        std::fs::create_dir_all(pubdir.join("sub")).unwrap();
        std::fs::write(pubdir.join("note.txt"), b"HELLO").unwrap();
        std::fs::write(pubdir.join("sub").join("a.css"), b".x{}").unwrap();
        // 目录外的敏感文件
        std::fs::write(dir.join("secret.txt"), b"SECRET").unwrap();

        let (bytes, ct) = read_public_file(&pubdir, "note.txt").expect("应能读到 public 文件");
        assert_eq!(bytes, b"HELLO");
        assert!(ct.contains("text/plain"));

        let (_, ct2) = read_public_file(&pubdir, "sub/a.css").expect("子目录也应可读");
        assert!(ct2.contains("text/css"));

        // 目录穿越必须被拒（否则 dev server 可读到项目任意文件）
        assert!(
            read_public_file(&pubdir, "../secret.txt").is_none(),
            "目录穿越应被拒绝"
        );
        assert!(read_public_file(&pubdir, "nope.txt").is_none());
        // 目录本身不是文件
        assert!(read_public_file(&pubdir, "sub").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
struct StartedServer {
    url: String,
    handle: actix_web::dev::ServerHandle,
}

struct ServerInner {
    url: String,
    handle: actix_web::dev::ServerHandle,
    stop: Arc<AtomicBool>,
    join: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct ServerHandle {
    inner: Arc<ServerInner>,
}

impl ServerHandle {
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// Request shutdown without joining worker threads. Safe for language-runtime finalizers.
    pub fn request_close(&self) {
        self.inner.stop.store(true, Ordering::Release);
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let handle = self.inner.handle.clone();
            if std::thread::Builder::new()
                .name("wake-dev-shutdown".to_string())
                .spawn(move || {
                    actix_web::rt::System::new().block_on(handle.stop(false));
                })
                .is_err()
            {
                self.inner.closed.store(false, Ordering::Release);
            }
        }
    }

    pub fn close(&self) -> std::io::Result<()> {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            self.inner.stop.store(true, Ordering::Release);
            actix_web::rt::System::new().block_on(self.inner.handle.stop(true));
        }
        self.join()
    }

    pub fn wait(&self) -> std::io::Result<()> {
        self.join()
    }

    fn join(&self) -> std::io::Result<()> {
        let mut join = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match join.take() {
            Some(join) => join
                .join()
                .map_err(|_| std::io::Error::other("Wake dev server thread panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for ServerInner {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if !self.closed.swap(true, Ordering::AcqRel) {
            let handle = self.handle.clone();
            let _ = std::thread::Builder::new()
                .name("wake-dev-shutdown".to_string())
                .spawn(move || {
                    actix_web::rt::System::new().block_on(handle.stop(false));
                });
        }
    }
}

pub fn start(root: &Path, port: u16, options: ServeOptions) -> std::io::Result<ServerHandle> {
    let root = root.to_path_buf();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let (started_tx, started_rx) = mpsc::channel();
    let error_tx = started_tx.clone();
    let join = std::thread::Builder::new()
        .name("wake-dev-server".to_string())
        .spawn(move || {
            let result = run_server(&root, port, options, started_tx, thread_stop);
            if let Err(error) = &result {
                let _ = error_tx.send(Err(error.to_string()));
            }
            result
        })?;
    let started = started_rx
        .recv()
        .map_err(|_| std::io::Error::other("Wake dev server exited during startup"))?
        .map_err(std::io::Error::other)?;
    Ok(ServerHandle {
        inner: Arc::new(ServerInner {
            url: started.url,
            handle: started.handle,
            stop,
            join: Mutex::new(Some(join)),
            closed: AtomicBool::new(false),
        }),
    })
}
