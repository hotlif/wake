//! wake_dev_server — Dev Server + HMR（DESIGN §7 / PLAN Phase 5）。
//!
//! `wake dev <root>`：以 actix-web 起 HTTP 服务，从**内存**服务增量打包产物；notify 监听源码变更
//! （75ms 静默窗口防抖）→ `IncrementalBundler` 增量重建 → 经 WebSocket 广播事件 → 浏览器 client runtime
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
    Arc, Condvar, Mutex, RwLock,
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
const WATCH_SETTLE_QUIET: Duration = Duration::from_millis(75);

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
    updated_modules: usize,
    cached_modules: usize,
    chunks: usize,
    assets: usize,
    duration: String,
    duration_ms: f64,
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
    mounts: Arc<Vec<Arc<MountedAppState>>>,
    /// HMR 事件广播（消息本身为 JSON 文本）。
    tx: broadcast::Sender<String>,
    /// 代理规则（已编译）；命中前缀的请求转发到后端 target。
    proxies: Arc<Vec<CompiledProxy>>,
}

struct MountedAppState {
    name: Option<String>,
    base_path: String,
    bundle: Arc<RwLock<BundleState>>,
    html: Arc<RwLock<String>>,
    public_dir: PathBuf,
    loading: Arc<MountLoadingState>,
}

#[derive(Debug, Clone)]
enum MountStatus {
    Pending,
    Loading,
    Loaded,
    Failed(String),
}

struct MountLoadingState {
    status: Mutex<MountStatus>,
    changed: Condvar,
    load_tx: mpsc::Sender<usize>,
    index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevLoading {
    Lazy,
    Eager,
}

pub struct MountedServeOptions {
    pub name: String,
    pub root: PathBuf,
    pub base_path: String,
    pub loading: DevLoading,
    pub entry: PathBuf,
    pub resolve_options: ResolveOptions,
    pub define: Vec<(String, String)>,
    pub target_env: TargetEnv,
    pub jsx_import_source: String,
    pub watch_roots: Vec<PathBuf>,
    pub before_rebuild: Option<BeforeRebuild>,
}

/// 文件变化后、BuildSession 失效前运行的生成钩子。返回需要一并失效的生成文件。
pub type BeforeRebuild =
    Arc<dyn Fn(&[PathBuf]) -> Result<Vec<PathBuf>, String> + Send + Sync + 'static>;
#[derive(Debug, Clone)]
pub enum ServerEvent {
    RebuildStart {
        changed_paths: Vec<PathBuf>,
        workspace: Option<String>,
        base_path: Option<String>,
    },
    Rebuilt {
        initial: bool,
        modules: usize,
        updated_modules: usize,
        cached_modules: usize,
        chunks: usize,
        assets: usize,
        duration_ms: f64,
        workspace: Option<String>,
        base_path: Option<String>,
    },
    Diagnostics {
        diagnostics: Vec<Diagnostic>,
    },
    WorkspaceState {
        total: usize,
        loaded: usize,
        failed: usize,
        current: Option<String>,
        failed_names: Vec<String>,
    },
    Closed,
}

pub type EventHandler = Arc<dyn Fn(ServerEvent) + Send + Sync + 'static>;

/// Dev server 选项（由 CLI 读 `wake.config.toml` 装配）。WAKE-COMPATIBILITY §M3。
pub struct ServeOptions {
    /// 已由调用方解析完成的入口文件。
    pub entry: PathBuf,
    /// URL base path owned by the primary application.
    pub base_path: String,
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
    /// Additional independently bundled applications mounted below this server.
    pub mounts: Vec<MountedServeOptions>,
}

impl Default for ServeOptions {
    fn default() -> ServeOptions {
        ServeOptions {
            entry: PathBuf::from("src/index.tsx"),
            base_path: "/".to_string(),
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
            mounts: Vec::new(),
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

struct MountSpec {
    name: Option<String>,
    root: PathBuf,
    base_path: String,
    loading: DevLoading,
    entry: PathBuf,
    resolve_options: ResolveOptions,
    define: Vec<(String, String)>,
    target_env: TargetEnv,
    jsx_import_source: String,
    watch_roots: Vec<PathBuf>,
    before_rebuild: Option<BeforeRebuild>,
}

fn normalize_mount_base(value: &str) -> std::io::Result<String> {
    if value.contains('\\') || value.contains('%') || value.contains('?') || value.contains('#') {
        return Err(std::io::Error::other(format!(
            "invalid Wake dev mount base path `{value}`"
        )));
    }
    let segments = value.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(std::io::Error::other(format!(
            "invalid Wake dev mount base path `{value}`"
        )));
    }
    Ok(if value.trim_matches('/').is_empty() {
        "/".to_string()
    } else {
        format!("/{}/", value.trim_matches('/'))
    })
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
        base_path,
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
        mounts,
    } = options;
    // 编译代理规则（pathRewrite 正则一次编译）。非法正则跳过并告警。
    let proxies: Vec<CompiledProxy> = proxy
        .into_iter()
        .filter_map(CompiledProxy::compile)
        .collect();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let base_path = normalize_mount_base(&base_path)?;
    let entry = if entry.is_absolute() {
        entry
    } else {
        root.join(entry)
    };
    let mut specs = vec![MountSpec {
        name: None,
        root: root.clone(),
        base_path: base_path.clone(),
        loading: DevLoading::Eager,
        entry,
        resolve_options,
        define,
        target_env,
        jsx_import_source,
        watch_roots,
        before_rebuild,
    }];
    for mount in mounts {
        let mount_root = mount
            .root
            .canonicalize()
            .unwrap_or_else(|_| mount.root.clone());
        let mount_base = normalize_mount_base(&mount.base_path)?;
        if !mount_base.starts_with(&base_path) || mount_base == base_path {
            return Err(std::io::Error::other(format!(
                "Wake dev mount `{}` at `{mount_base}` is outside primary base `{base_path}`",
                mount.name
            )));
        }
        let mount_entry = if mount.entry.is_absolute() {
            mount.entry
        } else {
            mount_root.join(mount.entry)
        };
        specs.push(MountSpec {
            name: Some(mount.name),
            root: mount_root,
            base_path: mount_base,
            loading: mount.loading,
            entry: mount_entry,
            resolve_options: mount.resolve_options,
            define: mount.define,
            target_env: mount.target_env,
            jsx_import_source: mount.jsx_import_source,
            watch_roots: mount.watch_roots,
            before_rebuild: mount.before_rebuild,
        });
    }
    for spec in &specs {
        if !spec.entry.is_file() {
            return Err(std::io::Error::other(format!(
                "entry file does not exist for Wake dev mount `{}`: {}",
                spec.name.as_deref().unwrap_or("site"),
                spec.entry.display()
            )));
        }
    }
    for index in 1..specs.len() {
        for other in index + 1..specs.len() {
            if specs[index].base_path.starts_with(&specs[other].base_path)
                || specs[other].base_path.starts_with(&specs[index].base_path)
            {
                return Err(std::io::Error::other(format!(
                    "overlapping Wake dev mounts `{}` and `{}`",
                    specs[index].base_path, specs[other].base_path
                )));
            }
        }
    }

    let sty = Sty::detect(quiet);
    let (tx, _rx) = broadcast::channel::<String>(64);
    let (load_tx, load_rx) = mpsc::channel::<usize>();
    let mounted_states = Arc::new(
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                Arc::new(MountedAppState {
                    name: spec.name.clone(),
                    base_path: spec.base_path.clone(),
                    bundle: Arc::new(RwLock::new(BundleState {
                        js: String::new(),
                        chunks: std::collections::HashMap::new(),
                        assets: std::collections::HashMap::new(),
                        map: None,
                        error: None,
                    })),
                    html: Arc::new(RwLock::new(load_html_template(
                        &spec.root,
                        &spec.base_path,
                        spec.name.as_deref(),
                    ))),
                    public_dir: spec.root.join("public"),
                    loading: Arc::new(MountLoadingState {
                        status: Mutex::new(if spec.loading == DevLoading::Lazy {
                            MountStatus::Pending
                        } else {
                            MountStatus::Loading
                        }),
                        changed: Condvar::new(),
                        load_tx: load_tx.clone(),
                        index,
                    }),
                })
            })
            .collect::<Vec<_>>(),
    );

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
    let (ready_tx, ready_rx) = mpsc::channel::<Result<Option<BuildSummary>, String>>();
    let watcher_stop = Arc::clone(&stop);
    let watcher_join = {
        let tx = tx.clone();
        let watcher_mounts = Arc::clone(&mounted_states);
        let watcher_events = event_handler.clone();
        std::thread::Builder::new()
            .name("wake-dev-watch".into())
            .spawn(move || {
                watch_and_rebuild(
                    specs,
                    watcher_mounts,
                    tx,
                    ready_tx,
                    sty,
                    load_rx,
                    watcher_stop,
                    watcher_events,
                );
            })
            .expect("spawn watcher thread")
    };
    // 等首次构建及所有监听目标注册完成再开始服务。这样 `start()` 返回即表示后续文件
    // 变化不会落入 watcher 尚未就绪的窗口。
    let summary = match ready_rx.recv() {
        Ok(Ok(summary)) => summary,
        Ok(Err(error)) => {
            stop.store(true, Ordering::Release);
            let _ = watcher_join.join();
            return Err(std::io::Error::other(error));
        }
        Err(error) => {
            stop.store(true, Ordering::Release);
            let _ = watcher_join.join();
            return Err(std::io::Error::other(format!(
                "Wake file watcher exited during startup: {error}"
            )));
        }
    };

    // 浏览器展示地址：0.0.0.0 时用 localhost。
    let display_host = if host == "0.0.0.0" {
        "localhost"
    } else {
        host.as_str()
    };
    let url = format!("http://{display_host}:{port}{base_path}");

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
        mounts: mounted_states,
        tx,
        proxies: Arc::new(proxies),
    });
    let server = HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            // 放宽负载上限，便于代理转发较大的 POST 请求体。
            .app_data(web::PayloadConfig::new(64 * 1024 * 1024))
            .route("/__wake/client.js", web::get().to(serve_client))
            .route("/__wake_hmr", web::get().to(ws_handler))
            // 默认服务：先试代理转发（任意方法），未命中且为 GET 则回退 SPA HTML。
            .default_service(web::to(serve_default))
    })
    .bind((host.as_str(), port));
    let server = match server {
        Ok(server) => server.workers(4).run(),
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
    specs: Vec<MountSpec>,
    mounts: Arc<Vec<Arc<MountedAppState>>>,
    tx: broadcast::Sender<String>,
    ready_tx: mpsc::Sender<Result<Option<BuildSummary>, String>>,
    sty: Sty,
    load_rx: mpsc::Receiver<usize>,
    stop: Arc<AtomicBool>,
    event_handler: Option<EventHandler>,
) {
    struct Worker {
        spec: MountSpec,
        session: Option<BuildSession>,
        watch_targets: Vec<(PathBuf, RecursiveMode)>,
    }

    let mut workers = specs
        .into_iter()
        .map(|spec| {
            let watch_targets = mount_watch_targets(&spec);
            Worker {
                spec,
                session: None,
                watch_targets,
            }
        })
        .collect::<Vec<_>>();
    let (evt_tx, evt_rx) = mpsc::channel::<(Vec<PathBuf>, bool)>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && is_source_event(&event)
            {
                let structural = is_structural_event(&event);
                let _ = evt_tx.send((event.paths, structural));
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                let message = format!("cannot create Wake file watcher: {error}");
                let _ = ready_tx.send(Err(message));
                return;
            }
        };
    let mut registered = std::collections::BTreeMap::<PathBuf, RecursiveMode>::new();
    for worker in &workers {
        for (path, mode) in &worker.watch_targets {
            let should_register = match registered.get(path) {
                Some(RecursiveMode::Recursive) => false,
                Some(RecursiveMode::NonRecursive) => *mode == RecursiveMode::Recursive,
                None => true,
            };
            if should_register {
                if let Err(error) = watcher.watch(path, *mode) {
                    let message = format!("cannot watch {}: {error}", path.display());
                    let _ = ready_tx.send(Err(message));
                    return;
                }
                registered.insert(path.clone(), *mode);
            }
        }
    }

    let mut primary_summary = None;
    for index in 0..workers.len() {
        if workers[index].spec.loading == DevLoading::Lazy {
            continue;
        }
        workers[index].session = Some(create_mount_session(&workers[index].spec));
        let worker = &mut workers[index];
        let summary = rebuild_mount(
            worker.session.as_mut().expect("created session"),
            &worker.spec,
            &mounts[index],
            &tx,
            true,
            sty,
            event_handler.as_ref(),
        );
        if index == 0 {
            primary_summary = summary;
            set_mount_status(&mounts[index], MountStatus::Loaded);
        } else if summary.is_some() {
            set_mount_status(&mounts[index], MountStatus::Loaded);
        } else {
            let error = mounts[index]
                .bundle
                .read()
                .unwrap()
                .error
                .clone()
                .unwrap_or_else(|| "workspace build failed".to_string());
            set_mount_status(&mounts[index], MountStatus::Failed(error));
        }
    }
    emit_workspace_state(&mounts, None, event_handler.as_ref());
    if ready_tx.send(Ok(primary_summary)).is_err() {
        return;
    }

    while !stop.load(Ordering::Acquire) {
        while let Ok(index) = load_rx.try_recv() {
            if index == 0 || index >= workers.len() || workers[index].session.is_some() {
                continue;
            }
            emit_workspace_state(
                &mounts,
                workers[index].spec.name.clone(),
                event_handler.as_ref(),
            );
            if let Some(regenerate) = &workers[index].spec.before_rebuild
                && let Err(error) = regenerate(&[])
            {
                mounts[index].bundle.write().unwrap().error = Some(error.clone());
                if let Some(handler) = &event_handler {
                    handler(ServerEvent::Diagnostics {
                        diagnostics: vec![Diagnostic::error(error.clone()).with_code("WAKE_BUILD")],
                    });
                }
                let _ = tx.send(msg_error(&error, workers[index].spec.name.as_deref()));
                set_mount_status(&mounts[index], MountStatus::Failed(error));
                emit_workspace_state(&mounts, None, event_handler.as_ref());
                continue;
            }
            workers[index].session = Some(create_mount_session(&workers[index].spec));
            let worker = &mut workers[index];
            let summary = rebuild_mount(
                worker.session.as_mut().expect("created session"),
                &worker.spec,
                &mounts[index],
                &tx,
                true,
                sty,
                event_handler.as_ref(),
            );
            if summary.is_some() {
                set_mount_status(&mounts[index], MountStatus::Loaded);
            } else {
                let error = mounts[index]
                    .bundle
                    .read()
                    .unwrap()
                    .error
                    .clone()
                    .unwrap_or_else(|| "workspace build failed".to_string());
                set_mount_status(&mounts[index], MountStatus::Failed(error));
            }
            emit_workspace_state(&mounts, None, event_handler.as_ref());
        }

        let (mut changed, mut structural) = match evt_rx.recv_timeout(Duration::from_millis(25)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        while let Ok((paths, event_structural)) = evt_rx.recv_timeout(WATCH_SETTLE_QUIET) {
            changed.extend(paths);
            structural |= event_structural;
        }
        changed.sort();
        changed.dedup();
        let config_changed = changed.iter().any(|path| {
            path.file_name()
                .is_some_and(|name| name == "wake.config.toml")
        });
        for (index, worker) in workers.iter().enumerate().skip(1) {
            let matches_mount = changed.iter().any(|path| {
                worker
                    .watch_targets
                    .iter()
                    .any(|(target, mode)| match mode {
                        RecursiveMode::Recursive => path.starts_with(target),
                        RecursiveMode::NonRecursive => {
                            path == target || path.parent() == Some(target.as_path())
                        }
                    })
            });
            if matches_mount
                && worker.session.is_none()
                && matches!(
                    &*mounts[index].loading.status.lock().unwrap(),
                    MountStatus::Failed(_)
                )
            {
                set_mount_status(&mounts[index], MountStatus::Pending);
            }
        }

        let affected = workers
            .iter()
            .enumerate()
            .filter(|(_, worker)| {
                worker.session.is_some()
                    && changed.iter().any(|path| {
                        worker
                            .watch_targets
                            .iter()
                            .any(|(target, mode)| match mode {
                                RecursiveMode::Recursive => path.starts_with(target),
                                RecursiveMode::NonRecursive => {
                                    path == target || path.parent() == Some(target.as_path())
                                }
                            })
                    })
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();

        for index in affected {
            let workspace = workers[index].spec.name.clone();
            let mount_base = workers[index].spec.base_path.clone();
            if let Some(handler) = &event_handler {
                handler(ServerEvent::RebuildStart {
                    changed_paths: changed.clone(),
                    workspace: workspace.clone(),
                    base_path: workspace.as_ref().map(|_| mount_base.clone()),
                });
            }
            let mut invalidated = changed.clone();
            if let Some(regenerate) = &workers[index].spec.before_rebuild {
                match regenerate(&changed) {
                    Ok(mut generated) => {
                        structural |= !generated.is_empty();
                        invalidated.append(&mut generated);
                        invalidated.sort();
                        invalidated.dedup();
                    }
                    Err(error) => {
                        mounts[index].bundle.write().unwrap().error = Some(error.clone());
                        if let Some(handler) = &event_handler {
                            handler(ServerEvent::Diagnostics {
                                diagnostics: vec![
                                    Diagnostic::error(error.clone()).with_code("WAKE_BUILD"),
                                ],
                            });
                        }
                        let _ = tx.send(msg_error(&error, workspace.as_deref()));
                        continue;
                    }
                }
            }
            *mounts[index].html.write().unwrap() = load_html_template(
                &workers[index].spec.root,
                &workers[index].spec.base_path,
                workers[index].spec.name.as_deref(),
            );
            let worker = &mut workers[index];
            if config_changed {
                worker.session = Some(create_mount_session(&worker.spec));
            } else {
                worker
                    .session
                    .as_mut()
                    .expect("loaded session")
                    .invalidate_paths(&invalidated, structural);
            }
            let session = worker.session.as_mut().expect("loaded session");
            let _ = rebuild_mount(
                session,
                &worker.spec,
                &mounts[index],
                &tx,
                false,
                sty,
                event_handler.as_ref(),
            );
        }
    }
}

fn mount_watch_targets(spec: &MountSpec) -> Vec<(PathBuf, RecursiveMode)> {
    let default_watch_dir = {
        let src = spec.root.join("src");
        if src.is_dir() { src } else { spec.root.clone() }
    };
    let mut targets = if spec.watch_roots.is_empty() {
        vec![(default_watch_dir, RecursiveMode::Recursive)]
    } else {
        spec.watch_roots
            .iter()
            .filter_map(|path| {
                let path = if path.is_absolute() {
                    path.clone()
                } else {
                    spec.root.join(path)
                };
                if path.is_dir() {
                    Some((path, RecursiveMode::Recursive))
                } else {
                    path.parent()
                        .map(|parent| (parent.to_path_buf(), RecursiveMode::NonRecursive))
                }
            })
            .collect::<Vec<_>>()
    };
    let public_dir = spec.root.join("public");
    if public_dir.is_dir() {
        targets.push((public_dir, RecursiveMode::Recursive));
    }
    targets.push((spec.root.clone(), RecursiveMode::NonRecursive));
    targets.sort_by(|left, right| left.0.cmp(&right.0));
    targets.dedup_by(|left, right| left.0 == right.0);
    targets
}

fn create_mount_session(spec: &MountSpec) -> BuildSession {
    let mut bundler = IncrementalBundler::new(Arc::new(OsFileSystem));
    bundler.set_project_root(spec.root.clone());
    // 别名（@/@@）+ define（dev 口径）须在首次 build 前设置，dev 与 build 一致。
    bundler.set_resolve_options(spec.resolve_options.clone());
    bundler.set_define(spec.define.clone());
    bundler.set_target_env(spec.target_env.clone());
    bundler.set_public_path(spec.base_path.clone());
    // dev 走非 minify 单包路径 → 可产出精确 sourcemap（WAKE-COMPATIBILITY §M4d）。
    bundler.enable_sourcemap();
    // 零运行时 CSS-in-JS（§M5）：dev 不抽取 `.css`，抽出的样式随模块体 `<style>` 注入，
    // 与 `.css` 模块的 dev 行为一致。项目未用 Crab CSS 时零开销。
    bundler.enable_css_in_js();
    // 代码分割：与 prod 行为一致——动态 `import()` 切出 async chunk。此前 dev 不开分割，
    // 懒加载模块被内联进单包（能跑但不懒加载），与生产产物结构不一致、掩盖分割相关问题。
    bundler.enable_code_splitting();
    // JSX **dev runtime**：`jsxDEV` 携带 `{fileName,lineNumber,columnNumber}`，
    // React DevTools 借此显示组件栈、报错能定位到源文件行列（保持既定行为 的 dev 口径）。
    // 该口径已混入 `content_key`，与 prod 的模块摘要缓存互不干扰。
    let import_source = Box::leak(spec.jsx_import_source.clone().into_boxed_str());
    bundler.set_jsx_runtime(true, import_source);
    BuildSession::from_incremental(bundler)
}

fn set_mount_status(mount: &MountedAppState, status: MountStatus) {
    *mount.loading.status.lock().unwrap() = status;
    mount.loading.changed.notify_all();
}

fn emit_workspace_state(
    mounts: &[Arc<MountedAppState>],
    current: Option<String>,
    handler: Option<&EventHandler>,
) {
    let Some(handler) = handler else { return };
    let mut loaded = 0;
    let mut failed_names = Vec::new();
    for mount in mounts.iter().skip(1) {
        match &*mount.loading.status.lock().unwrap() {
            MountStatus::Loaded => loaded += 1,
            MountStatus::Failed(_) => failed_names.push(
                mount
                    .name
                    .clone()
                    .unwrap_or_else(|| mount.base_path.clone()),
            ),
            MountStatus::Pending | MountStatus::Loading => {}
        }
    }
    failed_names.sort();
    handler(ServerEvent::WorkspaceState {
        total: mounts.len().saturating_sub(1),
        loaded,
        failed: failed_names.len(),
        current,
        failed_names,
    });
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
            | "toml"
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
fn rebuild_mount(
    session: &mut BuildSession,
    spec: &MountSpec,
    mount: &MountedAppState,
    tx: &broadcast::Sender<String>,
    first: bool,
    sty: Sty,
    event_handler: Option<&EventHandler>,
) -> Option<BuildSummary> {
    let t = Instant::now();
    let out = session.build_current_ref(BuildRequest::new(&spec.entry));
    let elapsed = t.elapsed();
    let dur = human_dur(elapsed);
    let sep = sty.dim("·");
    if out.has_errors() {
        let errs = out.diagnostics.iter().filter(|d| d.is_error()).count();
        let err = format_diagnostics(&out.diagnostics);
        {
            let mut s = mount.bundle.write().unwrap();
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
            let diagnostics = out
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_error())
                .cloned()
                .map(|mut diagnostic| {
                    if let Some(path) = diagnostic.path.as_deref() {
                        let path = PathBuf::from(path);
                        if !path.is_absolute() {
                            diagnostic.path =
                                Some(spec.root.join(path).to_string_lossy().into_owned());
                        }
                    }
                    if let Some(workspace) = &spec.name {
                        diagnostic
                            .notes
                            .push(format!("Docs workspace: {workspace}"));
                    }
                    diagnostic
                })
                .collect();
            handler(ServerEvent::Diagnostics { diagnostics });
        }
        let _ = tx.send(msg_error(&err, spec.name.as_deref()));
        None
    } else {
        let summary = BuildSummary {
            modules: out.module_count,
            updated_modules: out.updated_module_count,
            cached_modules: out.cached_module_count,
            chunks: out.chunks.len(),
            assets: out.assets.len(),
            duration: dur.clone(),
            duration_ms: elapsed.as_secs_f64() * 1000.0,
        };
        {
            let mut s = mount.bundle.write().unwrap();
            // 追加 sourceMappingURL 让 DevTools 自动拉取（外链 .map，不膨胀 bundle 体积）。
            let map = out.chunks[out.entry_chunk].source_map.clone();
            s.js = if map.is_some() {
                format!(
                    "{}\n//# sourceMappingURL={}bundle.js.map\n",
                    out.bundle, spec.base_path
                )
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
                "  {}  {}  {sep}  {}  {sep}  {}  {sep}  {}",
                sty.ok("✓"),
                sty.bold("已更新"),
                sty.accent(&format!("{} 模块", summary.updated_modules)),
                sty.dim(&format!("{} 缓存命中", summary.cached_modules)),
                sty.dim(&format!("耗时 {dur}")),
            );
        }
        if !first {
            let _ = tx.send(msg_reload(spec.name.as_deref()));
        }
        if let Some(handler) = event_handler {
            handler(ServerEvent::Rebuilt {
                initial: first,
                modules: summary.modules,
                updated_modules: summary.updated_modules,
                cached_modules: summary.cached_modules,
                chunks: summary.chunks,
                assets: summary.assets,
                duration_ms: summary.duration_ms,
                workspace: spec.name.clone(),
                base_path: spec.name.as_ref().map(|_| spec.base_path.clone()),
            });
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

async fn serve_client() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .body(CLIENT_RUNTIME)
}

/// 服务 HTML（含 SPA fallback：任何未知 GET 路径都回退到应用外壳）。
async fn serve_html(mount: &MountedAppState) -> HttpResponse {
    let html = mount.html.read().unwrap().clone();
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

    let Some(mount) = select_mount(&data.mounts, req.path()) else {
        return HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("wake dev: request is outside every configured mount");
    };
    if req.path() != "/" && format!("{}/", req.path()) == mount.base_path {
        return HttpResponse::PermanentRedirect()
            .insert_header(("Location", mount.base_path.clone()))
            .finish();
    }
    if let Err(error) = ensure_mount_ready(&mount) {
        return HttpResponse::ServiceUnavailable()
            .content_type("text/html; charset=utf-8")
            .insert_header(("Retry-After", "1"))
            .body(format!(
                "<!doctype html><meta charset=\"utf-8\"><title>Wake workspace unavailable</title><main style=\"font:14px/1.6 ui-monospace,monospace;padding:32px\"><h1>Workspace unavailable</h1><pre>{}</pre></main>",
                escape_html(&error)
            ));
    }
    let raw_rel = req
        .path()
        .strip_prefix(&mount.base_path)
        .unwrap_or_default();
    let Some(rel) = safe_request_relative(raw_rel) else {
        return HttpResponse::BadRequest()
            .content_type("text/plain; charset=utf-8")
            .body("wake dev: unsafe request path");
    };

    if rel == "bundle.js" {
        let js = mount.bundle.read().unwrap().js.clone();
        return HttpResponse::Ok()
            .content_type("application/javascript; charset=utf-8")
            .insert_header(("Cache-Control", "no-cache"))
            .body(js);
    }
    if rel == "bundle.js.map" {
        return match mount.bundle.read().unwrap().map.clone() {
            Some(map) => HttpResponse::Ok()
                .content_type("application/json; charset=utf-8")
                .insert_header(("Cache-Control", "no-cache"))
                .body(map),
            None => HttpResponse::NotFound().body("no source map"),
        };
    }

    // ② chunk（内存）
    if let Some(code) = mount.bundle.read().unwrap().chunks.get(&rel).cloned() {
        return HttpResponse::Ok()
            .content_type("application/javascript; charset=utf-8")
            .insert_header(("Cache-Control", "no-cache"))
            .body(code);
    }
    // ③ 资源产物（内存）
    if let Some(bytes) = mount.bundle.read().unwrap().assets.get(&rel).cloned() {
        return HttpResponse::Ok()
            .content_type(mime_for(&rel))
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes);
    }
    // ④ public/ 静态文件
    if let Some((bytes, ct)) = read_public_file(&mount.public_dir, &rel) {
        return HttpResponse::Ok()
            .content_type(ct)
            .insert_header(("Cache-Control", "no-cache"))
            .body(bytes);
    }
    // ⑤ SPA 回退：仅无扩展名的路径（前端路由），形似文件者 404。
    if looks_like_file(&rel) {
        return HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body(format!("wake dev: 未找到 `{}`", req.path()));
    }
    serve_html(&mount).await
}

fn select_mount(mounts: &[Arc<MountedAppState>], path: &str) -> Option<Arc<MountedAppState>> {
    mounts
        .iter()
        .filter(|mount| {
            path.starts_with(&mount.base_path)
                || (path != "/" && format!("{path}/") == mount.base_path)
        })
        .max_by_key(|mount| mount.base_path.len())
        .cloned()
}

fn ensure_mount_ready(mount: &MountedAppState) -> Result<(), String> {
    let mut status = mount.loading.status.lock().unwrap();
    loop {
        match &*status {
            MountStatus::Loaded => return Ok(()),
            MountStatus::Failed(error) => return Err(error.clone()),
            MountStatus::Pending => {
                *status = MountStatus::Loading;
                if mount.loading.load_tx.send(mount.loading.index).is_err() {
                    *status = MountStatus::Failed("Wake workspace loader stopped".to_string());
                    mount.loading.changed.notify_all();
                }
            }
            MountStatus::Loading => {
                status = mount.loading.changed.wait(status).unwrap();
            }
        }
    }
}

fn safe_request_relative(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = *bytes.get(index + 1)?;
            let low = *bytes.get(index + 2)?;
            decoded.push(hex_value(high)? * 16 + hex_value(low)?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    let decoded = String::from_utf8(decoded).ok()?;
    if decoded.contains('\\') || decoded.contains('\0') {
        return None;
    }
    if decoded
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return None;
    }
    Some(decoded.trim_start_matches('/').to_string())
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
    if std::fs::symlink_metadata(public_dir)
        .ok()?
        .file_type()
        .is_symlink()
    {
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
    let requested_mount = req
        .query_string()
        .split('&')
        .find_map(|pair| pair.strip_prefix("mount="))
        .and_then(safe_request_relative)
        .unwrap_or_default();
    let init = data
        .mounts
        .iter()
        .find(|mount| mount.name.as_deref().unwrap_or("") == requested_mount)
        .and_then(|mount| mount.bundle.read().unwrap().error.clone());

    actix_web::rt::spawn(async move {
        // 连接即同步当前状态。
        let first = match init {
            Some(err) => msg_error(
                &err,
                (!requested_mount.is_empty()).then_some(requested_mount.as_str()),
            ),
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
fn load_html_template(root: &Path, base_path: &str, mount: Option<&str>) -> String {
    let candidates = [root.join("public/index.html"), root.join("index.html")];
    for c in candidates {
        if let Ok(mut html) = std::fs::read_to_string(&c) {
            let mount = format!("\"{}\"", json_escape(mount.unwrap_or("")));
            let inject = format!(
                "<script>window.__WAKE_MOUNT__={mount}</script><script src=\"/__wake/client.js\"></script>"
            );
            if let Some(pos) = html.find("</head>") {
                html.insert_str(pos, &inject);
            } else {
                html.insert_str(0, &inject);
            }
            // 保证有 bundle 脚本引用。
            if !html.contains("bundle.js")
                && let Some(pos) = html.find("</body>")
            {
                html.insert_str(
                    pos,
                    &format!("<script src=\"{base_path}bundle.js\"></script>"),
                );
            }
            if base_path != "/" {
                html = html.replace(
                    "src=\"/bundle.js\"",
                    &format!("src=\"{base_path}bundle.js\""),
                );
            }
            return html;
        }
    }
    default_html(base_path, mount)
}

fn default_html(base_path: &str, mount: Option<&str>) -> String {
    let mount = format!("\"{}\"", json_escape(mount.unwrap_or("")));
    format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"/>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
         <title>wake dev</title>\
         <script>window.__WAKE_MOUNT__={mount}</script>\
         <script src=\"/__wake/client.js\"></script></head>\
         <body><div id=\"root\"></div><script src=\"{base_path}bundle.js\"></script></body></html>"
    )
}

/// 构造错误消息 JSON（转义 message）。
fn msg_error(err: &str, mount: Option<&str>) -> String {
    format!(
        r#"{{"type":"error","message":"{}","mount":{}}}"#,
        json_escape(err),
        json_mount_string(mount)
    )
}

fn msg_reload(mount: Option<&str>) -> String {
    format!(
        r#"{{"type":"reload","mount":{}}}"#,
        json_mount_string(mount)
    )
}

fn json_mount_string(value: Option<&str>) -> String {
    format!("\"{}\"", json_escape(value.unwrap_or("")))
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
    var mount = window.__WAKE_MOUNT__ || "";
    var ws = new WebSocket(proto + "://" + location.host + "/__wake_hmr?mount=" + encodeURIComponent(mount));
    ws.onmessage = function (e) {
      var m;
      try { m = JSON.parse(e.data); } catch (_) { return; }
      if (m.mount != null && m.mount !== mount) return;
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn http_get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(
                format!(
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn msg_error_is_valid_shape() {
        let m = msg_error("boom \"x\"\nline2", Some("rc-grid"));
        assert!(m.starts_with(r#"{"type":"error","message":""#));
        assert!(m.ends_with(r#""}"#));
        assert!(m.contains("\\\"x\\\""));
        assert!(m.contains(r#""mount":"rc-grid""#));
    }

    #[test]
    fn default_html_has_hooks() {
        let h = default_html("/docs/", Some("rc-grid"));
        assert!(h.contains("/__wake/client.js"));
        assert!(h.contains("/docs/bundle.js"));
        assert!(h.contains("id=\"root\""));
        assert!(h.contains("window.__WAKE_MOUNT__=\"rc-grid\""));
    }

    #[test]
    fn html_changes_are_watched() {
        assert!(is_watched_ext("html"));
    }

    #[test]
    fn request_paths_reject_encoded_and_backslash_traversal() {
        assert_eq!(
            safe_request_relative("assets/a.png"),
            Some("assets/a.png".into())
        );
        assert!(safe_request_relative("../secret").is_none());
        assert!(safe_request_relative("%2e%2e/secret").is_none());
        assert!(safe_request_relative("assets%5csecret").is_none());
        assert!(safe_request_relative("assets\\secret").is_none());
    }

    #[test]
    fn lazy_mounts_build_once_and_route_by_the_longest_base_path() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-mount-site-")
            .tempdir()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("wake-dev-mount-workspace-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/index.js"),
            "globalThis.__site_marker = 'site';",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("src/index.js"),
            "globalThis.__workspace_marker = 'rc-grid';",
        )
        .unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let events = Arc::new(Mutex::new(Vec::<ServerEvent>::new()));
        let captured = Arc::clone(&events);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                event_handler: Some(Arc::new(move |event| {
                    captured.lock().unwrap().push(event);
                })),
                mounts: vec![MountedServeOptions {
                    name: "rc-grid".to_string(),
                    root: workspace.path().to_path_buf(),
                    base_path: "/components/rc-grid/workbench/".to_string(),
                    loading: DevLoading::Lazy,
                    entry: workspace.path().join("src/index.js"),
                    resolve_options: ResolveOptions::default(),
                    define: Vec::new(),
                    target_env: TargetEnv::default(),
                    jsx_import_source: "react".to_string(),
                    watch_roots: vec![workspace.path().join("src")],
                    before_rebuild: None,
                }],
                ..ServeOptions::default()
            },
        )
        .unwrap();

        let site_route = http_get(port, "/components/rc-grid/");
        assert!(site_route.starts_with("HTTP/1.1 200"), "{site_route}");
        assert!(site_route.contains("window.__WAKE_MOUNT__=\"\""));

        let first = std::thread::spawn(move || http_get(port, "/components/rc-grid/workbench/"));
        let second =
            std::thread::spawn(move || http_get(port, "/components/rc-grid/workbench/bundle.js"));
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        assert!(first.contains("window.__WAKE_MOUNT__=\"rc-grid\""));
        assert!(second.starts_with("HTTP/1.1 200"), "{second}");
        assert!(second.contains("__workspace_marker"));

        let missing = http_get(port, "/components/rc-grid/workbench/missing.js");
        assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
        let captured = events.lock().unwrap();
        assert_eq!(
            captured
                .iter()
                .filter(|event| matches!(
                    event,
                    ServerEvent::Rebuilt {
                        initial: true,
                        workspace: Some(workspace),
                        ..
                    } if workspace == "rc-grid"
                ))
                .count(),
            1
        );
        assert!(captured.iter().any(|event| matches!(
            event,
            ServerEvent::WorkspaceState {
                total: 1,
                loaded: 1,
                failed: 0,
                ..
            }
        )));
        drop(captured);
        server.close().unwrap();
    }

    #[test]
    fn many_lazy_mount_descriptors_do_not_build_at_startup() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-many-lazy-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/index.js"),
            "export const site = true;",
        )
        .unwrap();
        let mounts = (0..51)
            .map(|index| MountedServeOptions {
                name: format!("rc-{index:02}"),
                root: root.path().to_path_buf(),
                base_path: format!("/components/rc-{index:02}/workbench/"),
                loading: DevLoading::Lazy,
                entry: root.path().join("src/index.js"),
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_string(),
                watch_roots: vec![root.path().join("src")],
                before_rebuild: None,
            })
            .collect();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let events = Arc::new(Mutex::new(Vec::<ServerEvent>::new()));
        let captured = Arc::clone(&events);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                mounts,
                event_handler: Some(Arc::new(move |event| {
                    captured.lock().unwrap().push(event);
                })),
                ..ServeOptions::default()
            },
        )
        .unwrap();
        let captured = events.lock().unwrap();
        assert_eq!(
            captured
                .iter()
                .filter(|event| matches!(event, ServerEvent::Rebuilt { .. }))
                .count(),
            1,
            "only the primary application should build at startup"
        );
        assert!(captured.iter().any(|event| matches!(
            event,
            ServerEvent::WorkspaceState {
                total: 51,
                loaded: 0,
                failed: 0,
                ..
            }
        )));
        drop(captured);
        server.close().unwrap();
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
            actix_web::rt::System::new().block_on(self.inner.handle.stop(false));
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
