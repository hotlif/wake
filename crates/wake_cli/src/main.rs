//! Wake CLI 入口（bin: `wake`）。
//!
//! Phase 0：命令骨架（`build` / `dev` / `parse` / `tokenize`）。`build <entry>` 能读文件
//! 并渲染一条带源码上下文的诊断，验证 wake_common 的诊断链路（PLAN §0.3 / Gate-0）。
//! 真正的编译/打包在 P1+ 逐步接入。

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wake_common::{FileSystem, OsFileSystem, RenderStyle, SourceFile, render};

mod ui;
use ui::{Ui, human_bytes, human_dur};

#[derive(Parser)]
#[command(name = "wake", version, about = "高性能 Rust Web 构建器", long_about = None)]
struct Cli {
    /// 强制关闭彩色输出（也遵循环境变量 NO_COLOR）。
    #[arg(long, global = true)]
    no_color: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 构建应用（读 `wake.config.toml`、组件扫描、别名、产出 dist + index.html）。
    Build {
        /// 入口文件路径。省略则由配置驱动：生成虚拟入口 `import("@/entry.tsx")`（保持既定行为 `app:build`）。
        entry: Option<PathBuf>,
        /// 输出目录。
        #[arg(long, default_value = "dist")]
        outdir: PathBuf,
        /// 启用持久化构建缓存（`.wake/cache.bin`）：全新进程冷构建跳过未变模块的 parse+codegen（PLAN §7.1）。
        #[arg(long)]
        cache: bool,
        /// 监听源码变更，进程常驻热重建（引擎保持温热，增量重建远快于每次冷起）。
        #[arg(long)]
        watch: bool,
        /// 产出 Source Map（`<chunk>.js.map` + `sourceMappingURL`）。
        ///
        /// 注意：当前仅**非压缩**产物支持精确映射，故本选项会关闭 minify/mangle
        /// （压缩路径会重排改写模块体，映射会错位）。用于调试生产构建的模块组合问题。
        #[arg(long)]
        sourcemap: bool,
    },
    /// 启动 Dev Server + HMR（Phase 5，actix-web）。
    Dev {
        /// 项目根目录。
        #[arg(default_value = ".")]
        root: PathBuf,
        /// 入口文件。优先于 `wake.config.toml` 的 `html.entry`。
        #[arg(long)]
        entry: Option<PathBuf>,
        /// 监听端口。
        #[arg(long, default_value_t = 5173)]
        port: u16,
    },
    /// 构建或开发 React 组件文档站。
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
    },
    /// 解析并打印 AST（Phase 2）。
    Parse {
        /// 源文件路径。
        file: PathBuf,
        /// 以 JSON 输出 AST。
        #[arg(long)]
        ast: bool,
    },
    /// 词法分析并打印 token 流（Phase 1）。
    Tokenize {
        /// 源文件路径。
        file: PathBuf,
    },
}

#[derive(Subcommand)]
enum DocsCommand {
    /// 启动文档开发服务器与增量 MDX/API/Demo 再生成。
    Dev {
        /// 项目根目录。
        #[arg(default_value = ".")]
        root: PathBuf,
        /// 监听端口；省略时使用配置或 5173。
        #[arg(long)]
        port: Option<u16>,
    },
    /// 生成可部署的静态文档站。
    Build {
        /// 项目根目录。
        #[arg(default_value = ".")]
        root: PathBuf,
        /// 输出目录；相对路径以项目根为基准。
        #[arg(long)]
        outdir: Option<PathBuf>,
        /// 部署公共路径，优先于 [docs].base_path。
        #[arg(long, value_name = "PATH")]
        base: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let style = resolve_style(cli.no_color);

    let result = match cli.command {
        Command::Build {
            entry,
            outdir,
            cache,
            watch,
            sourcemap,
        } => {
            if watch {
                cmd_build_watch(
                    entry.as_deref(),
                    &outdir,
                    cache,
                    sourcemap,
                    Ui::new(style.color),
                )
            } else {
                cmd_build(
                    entry.as_deref(),
                    &outdir,
                    cache,
                    sourcemap,
                    Ui::new(style.color),
                )
            }
        }
        Command::Dev { root, entry, port } => {
            cmd_dev(&root, entry.as_deref(), port, Ui::new(style.color))
        }
        Command::Docs { command } => match command {
            DocsCommand::Dev { root, port } => cmd_docs_dev(&root, port, Ui::new(style.color)),
            DocsCommand::Build { root, outdir, base } => cmd_docs_build(
                &root,
                outdir.as_deref(),
                base.as_deref(),
                Ui::new(style.color),
            ),
        },
        Command::Parse { file, ast } => cmd_parse(&file, ast, style),
        Command::Tokenize { file } => cmd_tokenize(&file, style),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// 是否着色：`--no-color` / `NO_COLOR` 环境变量优先，否则看 stderr 是否 tty。
fn resolve_style(no_color_flag: bool) -> RenderStyle {
    let disabled = no_color_flag || std::env::var_os("NO_COLOR").is_some();
    if disabled || !std::io::stderr().is_terminal() {
        RenderStyle::plain()
    } else {
        RenderStyle::colored()
    }
}

fn print_app_error(ui: &Ui, error: &wake_app::WakeError) -> ExitCode {
    eprintln!(
        "  {}  [{}] {}",
        ui.err("✗"),
        ui.err(&error.code),
        error.message
    );
    if let Some(path) = &error.path {
        eprintln!("    {} {}", ui.dim("→"), ui.dim(path));
    }
    for diagnostic in &error.diagnostics {
        eprintln!(
            "    {} {}",
            ui.warn(&diagnostic.severity),
            diagnostic.message
        );
        for note in &diagnostic.notes {
            eprintln!("      {} {note}", ui.dim("·"));
        }
    }
    ExitCode::FAILURE
}

fn print_app_result(ui: &Ui, label: &str, result: &wake_app::BuildResult) {
    let bytes = result.files.iter().map(|file| file.bytes).sum::<usize>();
    let duration = std::time::Duration::from_secs_f64(result.duration_ms / 1000.0);
    println!(
        "  {}  {}  {}  {}  {}  {}  {}  {}",
        ui.ok("✓"),
        ui.bold(label),
        ui.dim("·"),
        ui.accent(&format!("{} 模块", result.module_count)),
        ui.dim("·"),
        ui.accent(&human_bytes(bytes)),
        ui.dim("·"),
        ui.accent(&human_dur(duration)),
    );
    if let Some(output_dir) = &result.output_dir {
        println!("    {} {}", ui.dim("→"), ui.dim(output_dir));
    }
    for diagnostic in &result.diagnostics {
        println!(
            "    {} {}",
            ui.warn(&diagnostic.severity),
            diagnostic.message
        );
    }
    println!();
    let _ = std::io::stdout().flush();
}

fn cmd_build(
    entry: Option<&Path>,
    outdir: &Path,
    cache: bool,
    sourcemap: bool,
    ui: Ui,
) -> Result<(), ExitCode> {
    print_banner(&ui, "build");
    let options = wake_app::BuildOptions {
        project: wake_app::ProjectOptions {
            cwd: std::env::current_dir().ok(),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        outdir: Some(outdir.to_path_buf()),
        cache,
        source_map: sourcemap,
        write: true,
    };
    match wake_app::build(options, &wake_app::CancellationToken::default()) {
        Ok(result) => {
            print_app_result(&ui, "构建成功", &result);
            Ok(())
        }
        Err(error) => Err(print_app_error(&ui, &error)),
    }
}
/// `wake build --watch`：进程常驻，引擎保持温热。首次冷构建后监听源码，改动即**增量**热重建
/// 并写盘。省掉每次冷起的进程启动 + 构造（线程池）+ 缓存载入——热重建远快于一次性 `wake build`。
fn cmd_build_watch(
    entry: Option<&Path>,
    outdir: &Path,
    cache: bool,
    sourcemap: bool,
    ui: Ui,
) -> Result<(), ExitCode> {
    use std::sync::mpsc;
    use std::time::Duration;

    use notify::{RecursiveMode, Watcher};

    print_banner(&ui, "build --watch");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let context = wake_app::BuildContext::create(wake_app::BuildOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(cwd.clone()),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        outdir: Some(outdir.to_path_buf()),
        cache,
        source_map: sourcemap,
        write: true,
    })
    .map_err(|error| print_app_error(&ui, &error))?;
    match context.rebuild(Vec::new(), wake_app::CancellationToken::default()) {
        Ok(result) => print_app_result(&ui, "首次构建", &result),
        Err(error) => {
            context.close();
            return Err(print_app_error(&ui, &error));
        }
    }

    let watch_dir = {
        let src = cwd.join("src");
        if src.is_dir() { src } else { cwd }
    };
    let (tx, rx) = mpsc::channel::<(Vec<PathBuf>, bool)>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res
            && is_source_event(&event)
        {
            let structural = is_structural_event(&event);
            let _ = tx.send((event.paths, structural));
        }
    })
    .map_err(|error| {
        eprintln!("  {} 无法创建文件监听器：{error}", ui.err("✗"));
        ExitCode::FAILURE
    })?;
    watcher
        .watch(&watch_dir, RecursiveMode::Recursive)
        .map_err(|error| {
            eprintln!(
                "  {} 无法监听 {}：{error}",
                ui.err("✗"),
                watch_dir.display()
            );
            ExitCode::FAILURE
        })?;
    let shutdown = shutdown_signals().map_err(|error| {
        eprintln!("  {} 无法安装信号处理器：{error}", ui.err("✗"));
        ExitCode::FAILURE
    })?;
    println!(
        "    {} {} … (Ctrl-C 退出)",
        ui.dim("监听"),
        watch_dir.display()
    );

    loop {
        if let Ok(exit_code) = shutdown.try_recv() {
            context.close();
            return Err(ExitCode::from(exit_code));
        }
        let (mut changed, _structural) = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        std::thread::sleep(Duration::from_millis(30));
        while let Ok((paths, _)) = rx.recv_timeout(Duration::from_millis(20)) {
            changed.extend(paths);
        }
        changed.sort();
        changed.dedup();
        match context.rebuild(changed, wake_app::CancellationToken::default()) {
            Ok(result) => print_app_result(&ui, "热重建", &result),
            Err(error) => {
                let _ = print_app_error(&ui, &error);
            }
        }
    }
    context.close();
    Ok(())
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
/// 除源码外必须包含**图片与字体**：它们既可能被 JS `import`，也可能被 CSS 的 `url()` 引用，
/// 两条路径都会把字节内容（base64 或内容 hash 文件名）打进产物——换一张图不重建就是陈旧产物。
fn is_watched_ext(e: &str) -> bool {
    matches!(
        e,
        "ts" | "tsx"
            | "js"
            | "jsx"
            | "mts"
            | "cts"
            | "json"
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

fn print_banner(ui: &Ui, sub: &str) {
    println!();
    println!(
        "  {} {} {}  {}",
        ui.warn("⚡"),
        ui.brand("wake"),
        ui.dim(&format!("v{}", wake_app::VERSION)),
        ui.dim(sub)
    );
    println!();
}

/// `wake dev`：启动 Dev Server + HMR（Phase 5，actix-web）。阻塞直到进程退出。
fn shutdown_signals() -> Result<std::sync::mpsc::Receiver<u8>, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGINT, SIGTERM};
        let mut signals = signal_hook::iterator::Signals::new([SIGINT, SIGTERM])
            .map_err(|error| error.to_string())?;
        std::thread::Builder::new()
            .name("wake-cli-signals".to_string())
            .spawn(move || {
                if let Some(signal) = signals.forever().next() {
                    let _ = sender.send(if signal == SIGINT { 130 } else { 143 });
                }
            })
            .map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    {
        ctrlc::set_handler(move || {
            let _ = sender.send(130);
        })
        .map_err(|error| error.to_string())?;
    }
    Ok(receiver)
}

fn wait_for_server(server: wake_app::DevServer, ui: &Ui) -> Result<(), ExitCode> {
    use std::time::Duration;

    let shutdown = shutdown_signals().map_err(|error| {
        eprintln!("  {} 无法安装信号处理器：{error}", ui.err("✗"));
        ExitCode::FAILURE
    })?;
    let waiter_server = server.clone();
    let waiter = std::thread::Builder::new()
        .name("wake-cli-server-wait".to_string())
        .spawn(move || waiter_server.wait_until_closed())
        .map_err(|error| {
            eprintln!("  {} 无法启动等待线程：{error}", ui.err("✗"));
            ExitCode::FAILURE
        })?;
    loop {
        if let Ok(exit_code) = shutdown.recv_timeout(Duration::from_millis(50)) {
            if let Err(error) = server.close() {
                let _ = print_app_error(ui, &error);
            }
            let _ = waiter.join();
            return Err(ExitCode::from(exit_code));
        }
        if waiter.is_finished() {
            return waiter
                .join()
                .map_err(|_| ExitCode::FAILURE)?
                .map_err(|error| print_app_error(ui, &error));
        }
    }
}

fn cmd_dev(root: &Path, entry: Option<&Path>, port: u16, ui: Ui) -> Result<(), ExitCode> {
    print_banner(&ui, "dev");
    let server = wake_app::start_dev_server(wake_app::DevServerOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(root.to_path_buf()),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        host: None,
        port: Some(port),
        open: None,
    })
    .map_err(|error| print_app_error(&ui, &error))?;
    println!("    {} {}", ui.dim("Local"), ui.accent(server.url()));
    wait_for_server(server, &ui)
}
fn cmd_docs_dev(root: &Path, port: Option<u16>, ui: Ui) -> Result<(), ExitCode> {
    print_banner(&ui, "docs dev");
    let server = wake_app::start_docs_dev_server(wake_app::DevServerOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(root.to_path_buf()),
            config_path: None,
        },
        entry: None,
        host: None,
        port,
        open: None,
    })
    .map_err(|error| print_app_error(&ui, &error))?;
    println!("    {} {}", ui.dim("Local"), ui.accent(server.url()));
    wait_for_server(server, &ui)
}
fn cmd_docs_build(
    root: &Path,
    outdir: Option<&Path>,
    base: Option<&str>,
    ui: Ui,
) -> Result<(), ExitCode> {
    print_banner(&ui, "docs build");
    let result = wake_app::build_docs(
        wake_app::DocsBuildOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            outdir: outdir.map(Path::to_path_buf),
            base_path: base.map(str::to_string),
        },
        &wake_app::CancellationToken::default(),
    )
    .map_err(|error| print_app_error(&ui, &error))?;
    print_app_result(&ui, "文档构建成功", &result.build);
    println!("    {} {} routes", ui.dim("→"), result.routes.len());
    Ok(())
}
fn cmd_parse(file: &Path, ast: bool, style: RenderStyle) -> Result<(), ExitCode> {
    let fs = OsFileSystem;
    let src = match fs.read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: 无法读取 `{}`：{e}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };

    // 源类型：.cjs → 脚本，其余按模块。
    let source_type = if file.extension().is_some_and(|e| e == "cjs") {
        wake_ecma_ast::SourceType::Script
    } else {
        wake_ecma_ast::SourceType::Module
    };

    let interner = wake_common::Interner::new();
    let out = wake_ecma_parser::parse(&src, &interner, source_type);

    // 统计 + 依赖。
    let stmt_count = out.module.with_ast(|p| p.body.len());
    println!(
        "解析 {} —— 顶层语句 {stmt_count} 条，依赖 {} 条",
        file.display(),
        out.dependencies.len()
    );
    for dep in &out.dependencies {
        println!("  {:?}  {}", dep.kind, interner.resolve(dep.specifier));
    }

    // 语义：作用域 / 符号 / 引用（2.5）。
    let model = out.module.with_ast(wake_ecma_parser::analyze);
    println!(
        "作用域 {} 个，符号 {} 个，未解析（全局/未声明）引用 {} 处",
        model.scopes.len(),
        model.symbols.len(),
        model.unresolved_count()
    );

    // --ast：打印 AST 结构。
    if ast {
        out.module.with_ast(|p| {
            println!("\n{p:#?}");
        });
    }

    // 诊断。
    if !out.diagnostics.is_empty() {
        let source = SourceFile::new(file.display().to_string(), src);
        for d in &out.diagnostics {
            eprint!("{}", render(d, &source, style));
        }
        if out.has_errors() {
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

fn cmd_tokenize(file: &Path, style: RenderStyle) -> Result<(), ExitCode> {
    let fs = OsFileSystem;
    let src = match fs.read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: 无法读取 `{}`：{e}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };

    let (tokens, diags) = wake_ecma_lexer::tokenize(&src);

    for t in &tokens {
        if t.is_eof() {
            continue;
        }
        let text = &src[t.span.lo as usize..t.span.hi as usize];
        let nl = if t.newline_before { " ⏎" } else { "" };
        println!(
            "{:>5}..{:<5} {:<18} {:?}{}",
            t.span.lo,
            t.span.hi,
            t.kind.describe(),
            text,
            nl
        );
    }

    if !diags.is_empty() {
        let source = SourceFile::new(file.display().to_string(), src);
        for d in &diags {
            eprint!("{}", render(d, &source, style));
        }
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}
