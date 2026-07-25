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
use std::sync::{Arc, RwLock, mpsc};
use std::time::{Duration, Instant};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use futures_util::StreamExt as _;
use notify::{RecursiveMode, Watcher};
use tokio::sync::broadcast;

use wake_bundler::{IncrementalBundler, ResolveOptions};
use wake_common::{Diagnostic, OsFileSystem};

// —— 终端着色（tty + 非 NO_COLOR 时启用）——
const RESET: &str = "\x1b[0m";

#[derive(Clone, Copy)]
struct Sty {
    color: bool,
}
impl Sty {
    fn detect() -> Sty {
        Sty {
            color: std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none(),
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
        self.p("\x1b[1;35m", s)
    }
    fn ok(&self, s: &str) -> String {
        self.p("\x1b[32m", s)
    }
    fn err(&self, s: &str) -> String {
        self.p("\x1b[31m", s)
    }
    fn dim(&self, s: &str) -> String {
        self.p("\x1b[2m", s)
    }
    fn accent(&self, s: &str) -> String {
        self.p("\x1b[36m", s)
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
        format!("{:.0}ms", ms.max(1.0))
    } else {
        format!("{:.2}s", ms / 1000.0)
    }
}

/// 当前产物状态（跨线程共享）。
struct BundleState {
    /// 最近一次成功构建的 JS 产物。
    js: String,
    /// 若最近一次构建失败，格式化后的诊断文本；否则 `None`。
    error: Option<String>,
}

/// HTTP 处理器共享数据。
struct AppState {
    bundle: Arc<RwLock<BundleState>>,
    /// HMR 事件广播（消息本身为 JSON 文本）。
    tx: broadcast::Sender<String>,
    /// 注入了 HMR client 脚本的 HTML 外壳。
    html: String,
    /// 代理规则（已编译）；命中前缀的请求转发到后端 target。
    proxies: Arc<Vec<CompiledProxy>>,
}

/// Dev server 选项（由 CLI 读 `wake.config.toml` 装配）。CRUSTIFY-PARITY §M3。
pub struct ServeOptions {
    /// 解析选项（含别名 `@`/`@@`/`@@@`）。
    pub resolve_options: ResolveOptions,
    /// 编译期 define（dev 口径：`process.env.NODE_ENV → "development"` + 用户 `[define]`）。
    pub define: Vec<(String, String)>,
    /// 监听地址（缺省 `127.0.0.1`；设 `0.0.0.0` 可局域网访问）。
    pub host: String,
    /// 启动后自动打开浏览器。
    pub open: bool,
    /// 代理规则（转发匹配前缀的请求到后端 target，对齐 crustify `devServer.proxy`）。
    pub proxy: Vec<ProxyRule>,
}

impl Default for ServeOptions {
    fn default() -> ServeOptions {
        ServeOptions {
            resolve_options: ResolveOptions::default(),
            define: Vec::new(),
            host: "127.0.0.1".to_string(),
            open: false,
            proxy: Vec::new(),
        }
    }
}

/// 一条代理规则（对齐 crustify `Proxy`）。
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
    let ServeOptions {
        resolve_options,
        define,
        host,
        open,
        proxy,
    } = options;
    // 编译代理规则（pathRewrite 正则一次编译）。非法正则跳过并告警。
    let proxies: Vec<CompiledProxy> = proxy
        .into_iter()
        .filter_map(CompiledProxy::compile)
        .collect();
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let entry = match find_entry(&root) {
        Some(e) => e,
        None => {
            return Err(std::io::Error::other(format!(
                "未找到入口文件（在 {} 下找 src/index.{{tsx,ts,jsx,js}} 或 index.*）",
                root.display()
            )));
        }
    };
    let html = load_html_template(&root);

    let sty = Sty::detect();
    let bundle = Arc::new(RwLock::new(BundleState {
        js: String::new(),
        error: None,
    }));
    let (tx, _rx) = broadcast::channel::<String>(64);

    // 品牌行（在首次构建前打印）。
    println!();
    println!(
        "  {} {} {}",
        sty.warn("⚡"),
        sty.brand("wake dev"),
        sty.dim(&format!("v{}", env!("CARGO_PKG_VERSION"))),
    );
    println!();

    // —— 监听线程：独占 bundler，负责首次构建 + 增量重建 + 广播 ——
    let (ready_tx, ready_rx) = mpsc::channel::<()>();
    {
        let bundle = bundle.clone();
        let tx = tx.clone();
        let entry = entry.clone();
        let watch_root = root.clone();
        std::thread::Builder::new()
            .name("wake-dev-watch".into())
            .spawn(move || {
                watch_and_rebuild(
                    watch_root,
                    entry,
                    bundle,
                    tx,
                    ready_tx,
                    sty,
                    resolve_options,
                    define,
                );
            })
            .expect("spawn watcher thread");
    }
    // 等首次构建完成再开始服务（保证第一屏有产物）。
    let _ = ready_rx.recv();

    // 浏览器展示地址：0.0.0.0 时用 localhost。
    let display_host = if host == "0.0.0.0" {
        "localhost"
    } else {
        host.as_str()
    };
    let url = format!("http://{display_host}:{port}/");
    let entry_rel = entry
        .strip_prefix(&root)
        .unwrap_or(&entry)
        .display()
        .to_string();
    println!();
    println!(
        "  {}  {}   {}",
        sty.accent("➜"),
        sty.bold("本地:"),
        sty.accent(&url)
    );
    println!(
        "  {}  {}   {}",
        sty.accent("➜"),
        sty.bold("入口:"),
        sty.dim(&entry_rel)
    );
    if !proxies.is_empty() {
        for p in &proxies {
            println!(
                "  {}  {}   {} {} {}",
                sty.accent("➜"),
                sty.bold("代理:"),
                sty.dim(&p.context.join(",")),
                sty.dim("→"),
                sty.dim(&p.target)
            );
        }
    }
    println!();
    println!("  {}", sty.dim("监听中… 保存源码即热重载"));
    println!();

    // 自动打开浏览器（启动后）。
    if open {
        open_browser(&url);
    }

    let data = web::Data::new(AppState {
        bundle,
        tx,
        html,
        proxies: Arc::new(proxies),
    });
    actix_web::rt::System::new().block_on(async move {
        HttpServer::new(move || {
            App::new()
                .app_data(data.clone())
                // 放宽负载上限，便于代理转发较大的 POST 请求体。
                .app_data(web::PayloadConfig::new(64 * 1024 * 1024))
                .route("/bundle.js", web::get().to(serve_bundle))
                .route("/__wake/client.js", web::get().to(serve_client))
                .route("/__wake_hmr", web::get().to(ws_handler))
                // 默认服务：先试代理转发（任意方法），未命中且为 GET 则回退 SPA HTML。
                .default_service(web::to(serve_default))
        })
        .bind((host.as_str(), port))?
        .workers(2)
        .run()
        .await
    })
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
    tx: broadcast::Sender<String>,
    ready_tx: mpsc::Sender<()>,
    sty: Sty,
    resolve_options: ResolveOptions,
    define: Vec<(String, String)>,
) {
    let mut bundler = IncrementalBundler::new(Arc::new(OsFileSystem));
    // 别名（@/@@）+ define（dev 口径）须在首次 build 前设置，dev 与 build 一致。
    bundler.set_resolve_options(resolve_options);
    bundler.set_define(define);
    // 首次构建。
    rebuild(&mut bundler, &entry, &bundle, &tx, true, sty);
    let _ = ready_tx.send(());

    // notify：监听 src（存在则）否则根目录，避开 node_modules/dist 的海量文件。
    let watch_dir = {
        let src = root.join("src");
        if src.is_dir() { src } else { root.clone() }
    };
    let (evt_tx, evt_rx) = mpsc::channel::<()>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && is_source_event(&ev)
            {
                let _ = evt_tx.send(());
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                eprintln!("  {} 无法创建文件监听器：{e}", sty.err("✗"));
                return;
            }
        };
    if let Err(e) = watcher.watch(&watch_dir, RecursiveMode::Recursive) {
        eprintln!("  {} 无法监听 {}：{e}", sty.err("✗"), watch_dir.display());
        return;
    }

    loop {
        // 阻塞等第一个事件。
        if evt_rx.recv().is_err() {
            break;
        }
        // 落盘沉降：给 OS 少许时间完成写入（避免读到未 flush 的旧内容），
        // 再排空同批事件直到 20ms 静默（防抖）。
        std::thread::sleep(Duration::from_millis(30));
        while evt_rx.recv_timeout(Duration::from_millis(20)).is_ok() {}
        rebuild(&mut bundler, &entry, &bundle, &tx, false, sty);
    }
}

/// notify 事件是否为源码相关（忽略目录/元数据类噪声）。
fn is_source_event(ev: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && ev.paths.iter().any(|p| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e,
                "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" | "json" | "css"
            )
        })
    })
}

/// 执行一次（增量）构建并更新共享状态 + 广播 HMR 事件。
fn rebuild(
    bundler: &mut IncrementalBundler,
    entry: &Path,
    bundle: &Arc<RwLock<BundleState>>,
    tx: &broadcast::Sender<String>,
    first: bool,
    sty: Sty,
) {
    let t = Instant::now();
    let out = bundler.build(entry);
    let dur = human_dur(t.elapsed());
    let sep = sty.dim("·");
    if out.has_errors() {
        let errs = out.diagnostics.iter().filter(|d| d.is_error()).count();
        let err = format_diagnostics(&out.diagnostics);
        {
            let mut s = bundle.write().unwrap();
            s.error = Some(err.clone());
        }
        eprintln!(
            "  {}  {}  {sep}  {}",
            sty.err("✗"),
            sty.bold("构建失败"),
            sty.err(&format!("{errs} 个错误"))
        );
        for line in err.lines() {
            eprintln!("    {}", sty.dim(line));
        }
        let _ = tx.send(msg_error(&err));
    } else {
        {
            let mut s = bundle.write().unwrap();
            s.js = out.bundle;
            s.error = None;
        }
        let label = if first { "首次构建" } else { "热重建" };
        eprintln!(
            "  {}  {}  {sep}  {}  {sep}  {}",
            sty.ok("✓"),
            sty.bold(label),
            sty.accent(&format!("{} 模块", out.module_count)),
            sty.accent(&dur),
        );
        if !first {
            let _ = tx.send(r#"{"type":"reload"}"#.to_string());
        }
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

async fn serve_client() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .body(CLIENT_RUNTIME)
}

/// 服务 HTML（含 SPA fallback：任何未知 GET 路径都回退到应用外壳）。
async fn serve_html(data: web::Data<AppState>) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .insert_header(("Cache-Control", "no-cache"))
        .body(data.html.clone())
}

/// 默认服务：命中代理前缀 → 转发到后端（任意方法）；否则 GET 回退 SPA HTML、其它方法 404。
async fn serve_default(
    req: HttpRequest,
    body: web::Bytes,
    data: web::Data<AppState>,
) -> HttpResponse {
    if let Some(i) = data.proxies.iter().position(|p| p.matches(req.path())) {
        return forward(&req, body, &data.proxies[i]).await;
    }
    if req.method() == actix_web::http::Method::GET {
        serve_html(data).await
    } else {
        HttpResponse::NotFound().finish()
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

/// 查找入口：`src/index.{tsx,ts,jsx,js}` / `src/main.*` 优先，其次根目录同名文件。
fn find_entry(root: &Path) -> Option<PathBuf> {
    const NAMES: &[&str] = &[
        "src/index.tsx",
        "src/index.ts",
        "src/index.jsx",
        "src/index.js",
        "src/main.tsx",
        "src/main.ts",
        "index.tsx",
        "index.ts",
        "index.jsx",
        "index.js",
    ];
    NAMES.iter().map(|n| root.join(n)).find(|p| p.is_file())
}

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
