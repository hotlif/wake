//! Wake command-line frontend.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand, ValueEnum};
use wake_common::{FileSystem, OsFileSystem, RenderStyle, SourceFile, render};

mod dashboard;
mod ui;

use dashboard::{BuildMetrics, Dashboard, DashboardAction, DashboardState};
use ui::{OutputFormat, Ui, UiMode};

#[derive(Parser)]
#[command(name = "wake", version, about = "High-performance Rust web build tools", long_about = None)]
struct Cli {
    /// Disable terminal colors (also honors NO_COLOR).
    #[arg(long, global = true)]
    no_color: bool,

    /// Terminal UI mode for long-running commands.
    #[arg(long, global = true, value_enum, default_value_t = UiMode::Auto)]
    ui: UiMode,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build an application.
    Build {
        /// Optional entry file. Configuration is used when omitted.
        entry: Option<PathBuf>,
        /// Output directory.
        #[arg(long, default_value = "dist")]
        outdir: PathBuf,
        /// Enable the persistent build cache.
        #[arg(long)]
        cache: bool,
        /// Watch source files and rebuild continuously.
        #[arg(long)]
        watch: bool,
        /// Emit source maps (disables minification and code splitting).
        #[arg(long)]
        sourcemap: bool,
    },
    /// Start the application development server and HMR.
    Dev {
        /// Project root.
        #[arg(default_value = ".")]
        root: PathBuf,
        /// Entry file overriding wake.config.toml.
        #[arg(long)]
        entry: Option<PathBuf>,
        /// Listening port.
        #[arg(long, default_value_t = 5173)]
        port: u16,
    },
    /// Build or develop a React component documentation site.
    Docs {
        #[command(subcommand)]
        command: DocsCommand,
    },
    /// Parse a JavaScript or TypeScript source file.
    Parse {
        /// Source file.
        file: PathBuf,
        /// Print the raw debug AST.
        #[arg(long, conflicts_with = "format")]
        ast: bool,
        /// Output format; auto uses human output on a TTY and JSON in a pipe.
        #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
        format: OutputFormat,
    },
    /// Tokenize a JavaScript or TypeScript source file.
    Tokenize {
        /// Source file.
        file: PathBuf,
        /// Output format; auto uses human output on a TTY and JSON in a pipe.
        #[arg(long, value_enum, default_value_t = OutputFormat::Auto)]
        format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum DocsCommand {
    /// Start the documentation development server.
    Dev {
        /// Project root.
        #[arg(default_value = ".")]
        root: PathBuf,
        /// Listening port; configuration or 5173 is used when omitted.
        #[arg(long)]
        port: Option<u16>,
        /// Documentation site or component workbench.
        #[arg(long, value_enum, default_value_t = DocsModeArg::Site)]
        mode: DocsModeArg,
    },
    /// Generate a deployable static documentation site.
    Build {
        /// Project root.
        #[arg(default_value = ".")]
        root: PathBuf,
        /// Output directory relative to the project root.
        #[arg(long)]
        outdir: Option<PathBuf>,
        /// Public deployment path overriding [docs].base_path.
        #[arg(long, value_name = "PATH")]
        base: Option<String>,
        /// Documentation site or component workbench.
        #[arg(long, value_enum, default_value_t = DocsModeArg::Site)]
        mode: DocsModeArg,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum DocsModeArg {
    Site,
    Components,
}

impl From<DocsModeArg> for wake_app::DocsMode {
    fn from(value: DocsModeArg) -> Self {
        match value {
            DocsModeArg::Site => Self::Site,
            DocsModeArg::Components => Self::Components,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let ui = Ui::detect(cli.no_color);
    let style = if ui.color {
        RenderStyle::colored()
    } else {
        RenderStyle::plain()
    };

    let result = match cli.command {
        Command::Build {
            entry,
            outdir,
            cache,
            watch,
            sourcemap,
        } => {
            if watch {
                cmd_build_watch(entry.as_deref(), &outdir, cache, sourcemap, ui, cli.ui)
            } else {
                ensure_static_mode(cli.ui)
                    .and_then(|()| cmd_build(entry.as_deref(), &outdir, cache, sourcemap, ui))
            }
        }
        Command::Dev { root, entry, port } => cmd_dev(&root, entry.as_deref(), port, ui, cli.ui),
        Command::Docs { command } => match command {
            DocsCommand::Dev { root, port, mode } => cmd_docs_dev(&root, port, mode, ui, cli.ui),
            DocsCommand::Build {
                root,
                outdir,
                base,
                mode,
            } => ensure_static_mode(cli.ui)
                .and_then(|()| cmd_docs_build(&root, outdir.as_deref(), base.as_deref(), mode, ui)),
        },
        Command::Parse { file, ast, format } => {
            ensure_static_mode(cli.ui).and_then(|()| cmd_parse(&file, ast, format, style, ui))
        }
        Command::Tokenize { file, format } => {
            ensure_static_mode(cli.ui).and_then(|()| cmd_tokenize(&file, format, style, ui))
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

fn ensure_static_mode(mode: UiMode) -> Result<(), ExitCode> {
    if mode == UiMode::Tui {
        eprintln!("wake: --ui tui is only available for dev, docs dev, and build --watch");
        Err(ExitCode::FAILURE)
    } else {
        Ok(())
    }
}

fn use_tui(mode: UiMode) -> Result<bool, ExitCode> {
    match mode {
        UiMode::Plain => Ok(false),
        UiMode::Auto => Ok(Dashboard::supported()),
        UiMode::Tui if Dashboard::supported() => Ok(true),
        UiMode::Tui => {
            eprintln!(
                "wake: --ui tui requires interactive stdin and stderr and a capable terminal"
            );
            Err(ExitCode::FAILURE)
        }
    }
}

fn start_dashboard(
    mode: UiMode,
    ui: &Ui,
    state: &DashboardState,
) -> Result<Option<Dashboard>, ExitCode> {
    if !use_tui(mode)? {
        ui.header(&state.command);
        return Ok(None);
    }
    match Dashboard::new(ui.color) {
        Ok(mut dashboard) => {
            if let Err(error) = dashboard.draw(state) {
                dashboard.restore();
                if mode == UiMode::Auto {
                    eprintln!("wake: TUI unavailable ({error}); falling back to plain output");
                    ui.header(&state.command);
                    Ok(None)
                } else {
                    eprintln!("wake: failed to initialize TUI: {error}");
                    Err(ExitCode::FAILURE)
                }
            } else {
                Ok(Some(dashboard))
            }
        }
        Err(error) if mode == UiMode::Auto => {
            eprintln!("wake: TUI unavailable ({error}); falling back to plain output");
            ui.header(&state.command);
            Ok(None)
        }
        Err(error) => {
            eprintln!("wake: failed to initialize TUI: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn restore_for_error(
    dashboard: &mut Option<Dashboard>,
    ui: &Ui,
    command: &str,
    error: &wake_app::WakeError,
) -> ExitCode {
    if let Some(mut active) = dashboard.take() {
        active.restore();
        ui.header(command);
    }
    ui.app_error(error);
    ExitCode::FAILURE
}

fn metrics_from_result(result: &wake_app::BuildResult) -> BuildMetrics {
    BuildMetrics {
        modules: result.module_count,
        updated_modules: result.updated_module_count,
        cached_modules: result.cached_module_count,
        chunks: result
            .files
            .iter()
            .filter(|file| file.kind == "chunk")
            .count(),
        assets: result
            .files
            .iter()
            .filter(|file| file.kind == "asset" || file.kind == "css")
            .count(),
        duration_ms: result.duration_ms,
    }
}

fn cmd_build(
    entry: Option<&Path>,
    outdir: &Path,
    cache: bool,
    sourcemap: bool,
    ui: Ui,
) -> Result<(), ExitCode> {
    ui.header("build");
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
            ui.build_result("Built", &result, None);
            Ok(())
        }
        Err(error) => {
            ui.app_error(&error);
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_build_watch(
    entry: Option<&Path>,
    outdir: &Path,
    cache: bool,
    sourcemap: bool,
    ui: Ui,
    mode: UiMode,
) -> Result<(), ExitCode> {
    use std::sync::mpsc;

    use notify::{RecursiveMode, Watcher};

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let watch_dir = {
        let src = cwd.join("src");
        if src.is_dir() { src } else { cwd.clone() }
    };
    let mut state = DashboardState::new(
        "build --watch",
        &cwd,
        "WATCH",
        format!("{} · writing {}", watch_dir.display(), outdir.display()),
    );
    state.set_endpoint(watch_dir.display().to_string());
    let mut dashboard = start_dashboard(mode, &ui, &state)?;

    let context = match wake_app::BuildContext::create(wake_app::BuildOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(cwd.clone()),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        outdir: Some(outdir.to_path_buf()),
        cache,
        source_map: sourcemap,
        write: true,
    }) {
        Ok(context) => context,
        Err(error) => {
            return Err(restore_for_error(
                &mut dashboard,
                &ui,
                "build --watch",
                &error,
            ));
        }
    };

    match context.rebuild(Vec::new(), wake_app::CancellationToken::default()) {
        Ok(result) => {
            state.built(metrics_from_result(&result), true);
            if let Some(active) = dashboard.as_mut() {
                let _ = active.draw(&state);
            } else {
                ui.build_result("Initial build completed", &result, None);
                eprintln!(
                    "     {}  {}",
                    ui.dim("Watching"),
                    ui.accent(&watch_dir.display().to_string())
                );
                eprintln!();
            }
        }
        Err(error) => {
            context.close();
            return Err(restore_for_error(
                &mut dashboard,
                &ui,
                "build --watch",
                &error,
            ));
        }
    }

    let (tx, rx) = mpsc::channel::<Vec<PathBuf>>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res
                && is_source_event(&event)
            {
                let _ = tx.send(event.paths);
            }
        }) {
            Ok(watcher) => watcher,
            Err(error) => {
                context.close();
                if let Some(mut active) = dashboard.take() {
                    active.restore();
                    ui.header("build --watch");
                }
                eprintln!("  {}  Failed to create file watcher: {error}", ui.err("✗"));
                return Err(ExitCode::FAILURE);
            }
        };
    if let Err(error) = watcher.watch(&watch_dir, RecursiveMode::Recursive) {
        context.close();
        if let Some(mut active) = dashboard.take() {
            active.restore();
            ui.header("build --watch");
        }
        eprintln!(
            "  {}  Failed to watch {}: {error}",
            ui.err("✗"),
            watch_dir.display()
        );
        return Err(ExitCode::FAILURE);
    }
    let shutdown = shutdown_signals().map_err(|error| {
        if let Some(mut active) = dashboard.take() {
            active.restore();
        }
        eprintln!("wake: failed to install signal handler: {error}");
        ExitCode::FAILURE
    })?;

    loop {
        if let Ok(exit_code) = shutdown.try_recv() {
            let reason = if exit_code == 143 {
                "SIGTERM"
            } else {
                "Ctrl-C"
            };
            return finish_watch(
                &context,
                &mut dashboard,
                &mut state,
                &ui,
                &watch_dir,
                reason,
                Some(exit_code),
            );
        }

        if let Some(active) = dashboard.as_mut() {
            let _ = active.draw(&state);
            match active
                .read_action(&mut state, Duration::from_millis(50))
                .unwrap_or(DashboardAction::Continue)
            {
                DashboardAction::Quit => {
                    return finish_watch(
                        &context,
                        &mut dashboard,
                        &mut state,
                        &ui,
                        &watch_dir,
                        "q",
                        None,
                    );
                }
                DashboardAction::Interrupt => {
                    return finish_watch(
                        &context,
                        &mut dashboard,
                        &mut state,
                        &ui,
                        &watch_dir,
                        "Ctrl-C",
                        Some(130),
                    );
                }
                DashboardAction::Continue => {}
            }
        }

        let next = if dashboard.is_some() {
            rx.try_recv().ok()
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(paths) => Some(paths),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return finish_watch(
                        &context,
                        &mut dashboard,
                        &mut state,
                        &ui,
                        &watch_dir,
                        "watcher closed",
                        None,
                    );
                }
            }
        };
        let Some(mut changed) = next else {
            continue;
        };

        std::thread::sleep(Duration::from_millis(30));
        while let Ok(paths) = rx.try_recv() {
            changed.extend(paths);
        }
        changed.sort();
        changed.dedup();
        state.rebuilding(changed.len());
        if let Some(active) = dashboard.as_mut() {
            let _ = active.draw(&state);
        } else {
            ui.rebuild_start(changed.len());
        }

        match context.rebuild(changed, wake_app::CancellationToken::default()) {
            Ok(result) => {
                let metrics = metrics_from_result(&result);
                state.built(metrics, false);
                if dashboard.is_none() {
                    ui.rebuilt(metrics, false);
                }
            }
            Err(error) => {
                state.error(format!("[{}] {}", error.code, error.message));
                if dashboard.is_none() {
                    ui.app_error(&error);
                }
            }
        }
    }
}

fn finish_watch(
    context: &wake_app::BuildContext,
    dashboard: &mut Option<Dashboard>,
    state: &mut DashboardState,
    ui: &Ui,
    watch_dir: &Path,
    reason: &str,
    exit_code: Option<u8>,
) -> Result<(), ExitCode> {
    state.stopping(reason);
    if let Some(active) = dashboard.as_mut() {
        let _ = active.draw(state);
    }
    context.close();
    state.stopped();
    if let Some(mut active) = dashboard.take() {
        let _ = active.draw(state);
        active.restore();
    }
    ui.final_summary(
        "Watch stopped",
        "Watch",
        &watch_dir.display().to_string(),
        state.rebuilds,
        state.runtime(),
        reason,
    );
    match exit_code {
        Some(code) => Err(ExitCode::from(code)),
        None => Ok(()),
    }
}

fn is_source_event(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    ) && event.paths.iter().any(|path| {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(is_watched_ext)
    })
}

fn is_watched_ext(extension: &str) -> bool {
    matches!(
        extension,
        "ts" | "tsx"
            | "js"
            | "jsx"
            | "mts"
            | "cts"
            | "json"
            | "css"
            | "raw"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "svg"
            | "webp"
            | "avif"
            | "ico"
            | "bmp"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
    )
}

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

fn cmd_dev(
    root: &Path,
    entry: Option<&Path>,
    port: u16,
    ui: Ui,
    mode: UiMode,
) -> Result<(), ExitCode> {
    let mut state = DashboardState::new("dev", root, "LOCAL", "HMR · source maps · watching");
    let mut dashboard = start_dashboard(mode, &ui, &state)?;
    let server = match wake_app::start_dev_server(wake_app::DevServerOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(root.to_path_buf()),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        host: None,
        port: Some(port),
        open: None,
    }) {
        Ok(server) => server,
        Err(error) => {
            return Err(restore_for_error(&mut dashboard, &ui, "dev", &error));
        }
    };
    run_server(server, &ui, &mut dashboard, &mut state)
}

fn cmd_docs_dev(
    root: &Path,
    port: Option<u16>,
    docs_mode: DocsModeArg,
    ui: Ui,
    ui_mode: UiMode,
) -> Result<(), ExitCode> {
    let components = docs_mode == DocsModeArg::Components;
    let command = if components {
        "docs components"
    } else {
        "docs dev"
    };
    let watch = if components {
        "Demo · Controls · HMR · watching"
    } else {
        "MDX · HMR · search index · watching"
    };
    let mut state = DashboardState::new(command, root, "LOCAL", watch);
    let mut dashboard = start_dashboard(ui_mode, &ui, &state)?;
    let server = match wake_app::start_docs_dev_server_with_mode(
        wake_app::DevServerOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: None,
            host: None,
            port,
            open: None,
        },
        docs_mode.into(),
    ) {
        Ok(server) => server,
        Err(error) => {
            return Err(restore_for_error(&mut dashboard, &ui, command, &error));
        }
    };
    run_server(server, &ui, &mut dashboard, &mut state)
}

fn run_server(
    server: wake_app::DevServer,
    ui: &Ui,
    dashboard: &mut Option<Dashboard>,
    state: &mut DashboardState,
) -> Result<(), ExitCode> {
    state.set_endpoint(server.url().to_string());
    apply_server_events(&server, state, dashboard.is_none().then_some(ui));
    if let Some(active) = dashboard.as_mut() {
        let _ = active.draw(state);
    } else {
        ui.server_ready(server.url(), state.metrics);
    }

    let shutdown = shutdown_signals().map_err(|error| {
        if let Some(mut active) = dashboard.take() {
            active.restore();
        }
        eprintln!("wake: failed to install signal handler: {error}");
        ExitCode::FAILURE
    })?;
    let waiter_server = server.clone();
    let waiter = std::thread::Builder::new()
        .name("wake-cli-server-wait".to_string())
        .spawn(move || waiter_server.wait_until_closed())
        .map_err(|error| {
            if let Some(mut active) = dashboard.take() {
                active.restore();
            }
            eprintln!("wake: failed to start server waiter: {error}");
            ExitCode::FAILURE
        })?;

    loop {
        apply_server_events(&server, state, dashboard.is_none().then_some(ui));
        if let Some(active) = dashboard.as_mut() {
            let _ = active.draw(state);
            match active
                .read_action(state, Duration::from_millis(50))
                .unwrap_or(DashboardAction::Continue)
            {
                DashboardAction::Quit => {
                    return stop_server(server, waiter, ui, dashboard, state, "q", None);
                }
                DashboardAction::Interrupt => {
                    return stop_server(server, waiter, ui, dashboard, state, "Ctrl-C", Some(130));
                }
                DashboardAction::Continue => {}
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }

        if let Ok(exit_code) = shutdown.try_recv() {
            let reason = if exit_code == 143 {
                "SIGTERM"
            } else {
                "Ctrl-C"
            };
            return stop_server(
                server,
                waiter,
                ui,
                dashboard,
                state,
                reason,
                Some(exit_code),
            );
        }
        if waiter.is_finished() {
            let result = waiter.join().map_err(|_| ExitCode::FAILURE)?;
            apply_server_events(&server, state, dashboard.is_none().then_some(ui));
            state.stopped();
            if let Some(mut active) = dashboard.take() {
                let _ = active.draw(state);
                active.restore();
            }
            ui.final_summary(
                "Server stopped",
                &state.endpoint_label,
                &state.endpoint,
                state.rebuilds,
                state.runtime(),
                "server closed",
            );
            return result.map_err(|error| {
                ui.app_error(&error);
                ExitCode::FAILURE
            });
        }
    }
}

fn stop_server(
    server: wake_app::DevServer,
    waiter: std::thread::JoinHandle<Result<(), wake_app::WakeError>>,
    ui: &Ui,
    dashboard: &mut Option<Dashboard>,
    state: &mut DashboardState,
    reason: &str,
    exit_code: Option<u8>,
) -> Result<(), ExitCode> {
    state.stopping(reason);
    if let Some(active) = dashboard.as_mut() {
        let _ = active.draw(state);
    }
    let close_result = server.close();
    let _ = waiter.join();
    state.stopped();
    if let Some(mut active) = dashboard.take() {
        let _ = active.draw(state);
        active.restore();
    }
    if let Err(error) = &close_result {
        ui.app_error(error);
    }
    ui.final_summary(
        "Server stopped",
        &state.endpoint_label,
        &state.endpoint,
        state.rebuilds,
        state.runtime(),
        reason,
    );
    match exit_code {
        Some(code) => Err(ExitCode::from(code)),
        None if close_result.is_err() => Err(ExitCode::FAILURE),
        None => Ok(()),
    }
}

fn apply_server_events(
    server: &wake_app::DevServer,
    state: &mut DashboardState,
    plain_ui: Option<&Ui>,
) {
    for event in server.drain_events() {
        match event {
            wake_app::DevServerEvent::RebuildStart { changed_paths } => {
                state.rebuilding(changed_paths.len());
                if let Some(ui) = plain_ui {
                    ui.rebuild_start(changed_paths.len());
                }
            }
            wake_app::DevServerEvent::Rebuilt {
                initial,
                modules,
                updated_modules,
                cached_modules,
                chunks,
                assets,
                duration_ms,
            } => {
                let metrics = BuildMetrics {
                    modules,
                    updated_modules,
                    cached_modules,
                    chunks,
                    assets,
                    duration_ms,
                };
                state.built(metrics, initial);
                if let Some(ui) = plain_ui {
                    ui.rebuilt(metrics, initial);
                }
            }
            wake_app::DevServerEvent::Diagnostic { message } => {
                state.error(message.clone());
                if let Some(ui) = plain_ui {
                    ui.diagnostic(&message);
                }
            }
            wake_app::DevServerEvent::Closed => state.stopped(),
        }
    }
}

fn cmd_docs_build(
    root: &Path,
    outdir: Option<&Path>,
    base: Option<&str>,
    docs_mode: DocsModeArg,
    ui: Ui,
) -> Result<(), ExitCode> {
    let components = docs_mode == DocsModeArg::Components;
    ui.header(if components {
        "docs components build"
    } else {
        "docs build"
    });
    let result = wake_app::build_docs_with_mode(
        wake_app::DocsBuildOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            outdir: outdir.map(Path::to_path_buf),
            base_path: base.map(str::to_string),
        },
        docs_mode.into(),
        &wake_app::CancellationToken::default(),
    )
    .map_err(|error| {
        ui.app_error(&error);
        ExitCode::FAILURE
    })?;
    let extra = if components {
        format!("  {} {} demos", ui.dim("·"), result.demos.len())
    } else {
        format!("  {} {} routes", ui.dim("·"), result.routes.len())
    };
    ui.build_result(
        if components {
            "Component workbench built"
        } else {
            "Documentation built"
        },
        &result.build,
        Some(&extra),
    );
    Ok(())
}

fn cmd_parse(
    file: &Path,
    ast: bool,
    format: OutputFormat,
    style: RenderStyle,
    ui: Ui,
) -> Result<(), ExitCode> {
    let format = if ast {
        OutputFormat::Human
    } else {
        format.resolve()
    };
    if format == OutputFormat::Human {
        ui.header("parse");
    }
    let fs = OsFileSystem;
    let source_text = match fs.read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("wake: failed to read {}: {error}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };
    let source_type = match file.extension().and_then(|extension| extension.to_str()) {
        Some("cjs") => wake_ecma_ast::SourceType::Script,
        Some("ts" | "mts" | "cts") => wake_ecma_ast::SourceType::TypeScript,
        Some("tsx") => wake_ecma_ast::SourceType::Tsx,
        Some("jsx") => wake_ecma_ast::SourceType::Jsx,
        _ => wake_ecma_ast::SourceType::Module,
    };
    let interner = wake_common::Interner::new();
    let output = wake_ecma_parser::parse(&source_text, &interner, source_type);
    let statement_count = output.module.with_ast(|program| program.body.len());

    if format == OutputFormat::Json {
        let diagnostics = output
            .diagnostics
            .iter()
            .map(wake_app::DiagnosticInfo::from)
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "sourceBytes": source_text.len(),
            "statementCount": statement_count,
            "dependencies": output.dependencies.len(),
            "hasTopLevelAwait": output.has_top_level_await,
            "diagnostics": diagnostics,
        });
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!("Parsed {}", file.display());
        println!(
            "  Statements {:<8} Dependencies {}",
            statement_count,
            output.dependencies.len()
        );
        for dependency in &output.dependencies {
            println!(
                "  {:<18} {}",
                format!("{:?}", dependency.kind),
                interner.resolve(dependency.specifier)
            );
        }
        let model = output.module.with_ast(wake_ecma_parser::analyze);
        println!(
            "  Scopes {:<12} Symbols {:<10} Unresolved {}",
            model.scopes.len(),
            model.symbols.len(),
            model.unresolved_count()
        );
        if ast {
            output.module.with_ast(|program| println!("\n{program:#?}"));
        }
    }

    if !output.diagnostics.is_empty() {
        let source = SourceFile::new(file.display().to_string(), source_text);
        for diagnostic in &output.diagnostics {
            eprint!("{}", render(diagnostic, &source, style));
        }
        let _ = std::io::stderr().flush();
        if output.has_errors() {
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

fn cmd_tokenize(
    file: &Path,
    format: OutputFormat,
    style: RenderStyle,
    ui: Ui,
) -> Result<(), ExitCode> {
    let format = format.resolve();
    if format == OutputFormat::Human {
        ui.header("tokenize");
    }
    let fs = OsFileSystem;
    let source_text = match fs.read_to_string(file) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("wake: failed to read {}: {error}", file.display());
            return Err(ExitCode::FAILURE);
        }
    };
    let (tokens, diagnostics) = wake_ecma_lexer::tokenize(&source_text);

    if format == OutputFormat::Json {
        let token_values = tokens
            .iter()
            .filter(|token| !token.is_eof())
            .map(|token| {
                serde_json::json!({
                    "kind": format!("{:?}", token.kind),
                    "start": token.span.lo,
                    "end": token.span.hi,
                    "newlineBefore": token.newline_before,
                    "text": &source_text[token.span.lo as usize..token.span.hi as usize],
                })
            })
            .collect::<Vec<_>>();
        let diagnostic_values = diagnostics
            .iter()
            .map(wake_app::DiagnosticInfo::from)
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "tokens": token_values,
            "diagnostics": diagnostic_values,
        });
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    } else {
        println!("  START..END    KIND               TEXT");
        for token in tokens.iter().filter(|token| !token.is_eof()) {
            let text = &source_text[token.span.lo as usize..token.span.hi as usize];
            let newline = if token.newline_before { " ↵" } else { "" };
            println!(
                "  {:>5}..{:<5} {:<18} {:?}{}",
                token.span.lo,
                token.span.hi,
                token.kind.describe(),
                text,
                newline
            );
        }
    }

    if !diagnostics.is_empty() {
        let source = SourceFile::new(file.display().to_string(), source_text);
        for diagnostic in &diagnostics {
            eprint!("{}", render(diagnostic, &source, style));
        }
        let _ = std::io::stderr().flush();
        return Err(ExitCode::FAILURE);
    }
    Ok(())
}
