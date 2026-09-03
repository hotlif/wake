//! Wake command-line frontend.

#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum, error::ErrorKind};
use wake_common::{FileSystem, OsFileSystem, RenderStyle, SourceFile, render};

mod console;
mod dashboard;
mod ui;

use dashboard::{BuildMetrics, Dashboard, DashboardAction, DashboardState};
use ui::{OutputFormat, Ui, UiMode, format_diagnostic_plain};

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
        /// Emit source maps (compatible with minification; code splitting remains separately configured).
        #[arg(long)]
        sourcemap: bool,
    },
    /// Bundle one JavaScript or TypeScript entry into one library file.
    Bundle {
        /// Entry source file.
        entry: PathBuf,
        /// Exact output file.
        #[arg(long)]
        outfile: PathBuf,
        /// Bundle host platform.
        #[arg(long, value_enum)]
        platform: Option<BundlePlatformArg>,
        /// Entry module format. Defaults from platform when omitted.
        #[arg(long, value_enum)]
        format: Option<BundleFormatArg>,
        /// Runtime syntax target, for example node20.
        #[arg(long)]
        target: Option<String>,
        /// Bare package supplied by the runtime; may be repeated.
        #[arg(long)]
        external: Vec<String>,
        /// Minify the bundle.
        #[arg(long)]
        minify: bool,
        /// Emit a source map next to outfile.
        #[arg(long)]
        sourcemap: bool,
        /// Enable persistent cache.
        #[arg(long)]
        cache: bool,
        /// Explicit wake.config.toml path.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Build and generate component-library artifacts.
    Library {
        #[command(subcommand)]
        command: LibraryCommand,
    },
    /// Initialize Wake-native Module Federation project artifacts.
    Federation {
        #[command(subcommand)]
        command: FederationCommand,
    },
    /// Start the application development server and Live Reload.
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
    /// Run JavaScript and TypeScript tests with Wake's React-focused test runtime.
    Test {
        /// Path or glob filters applied after test discovery.
        patterns: Vec<String>,
        /// Project root containing wake.config.toml.
        #[arg(long, default_value = ".")]
        root: PathBuf,
        /// Only run tests whose full names contain this pattern.
        #[arg(long = "name-pattern")]
        name_pattern: Option<String>,
        /// Select a configured test project; may be repeated.
        #[arg(long = "project")]
        projects: Vec<String>,
        /// Override the configured execution environment.
        #[arg(long, value_enum)]
        environment: Option<TestEnvironmentArg>,
        /// Keep the test context open and rerun affected tests after changes.
        #[arg(long)]
        watch: bool,
        /// Run tests affected by source-control changes.
        #[arg(long, conflicts_with = "related")]
        changed: bool,
        /// Run tests related to one or more source paths.
        #[arg(long, value_name = "PATH", num_args = 1.., conflicts_with = "changed")]
        related: Vec<PathBuf>,
        /// Collect coverage.
        #[arg(long)]
        coverage: bool,
        /// Update accepted structural and screenshot snapshots.
        #[arg(long = "update-snapshots")]
        update_snapshots: bool,
        /// Execute suites serially.
        #[arg(long, conflicts_with = "workers")]
        serial: bool,
        /// Worker count: auto, a positive integer, or a percentage such as 50%.
        #[arg(long, value_parser = parse_test_workers, value_name = "COUNT")]
        workers: Option<String>,
        /// Stop after this many failing suites; --bail without a value means one.
        #[arg(long, num_args = 0..=1, default_missing_value = "1")]
        bail: Option<u32>,
        /// Execute one 1-based shard, for example 2/3.
        #[arg(long, value_parser = parse_test_shard)]
        shard: Option<String>,
        /// Deterministic test-order seed.
        #[arg(long)]
        seed: Option<String>,
        /// Shuffle test order deterministically using the seed.
        #[arg(long)]
        shuffle: bool,
        /// Select the built-in result reporter.
        #[arg(long, value_enum)]
        reporter: Option<TestReporterArg>,
        /// Write reporter output to this path.
        #[arg(long, requires = "reporter")]
        output: Option<PathBuf>,
        /// Exit successfully when discovery finds no tests.
        #[arg(long = "allow-no-tests")]
        allow_no_tests: bool,
        /// Explicit Chrome, Edge, or Chromium executable.
        #[arg(long = "browser-path")]
        browser_path: Option<PathBuf>,
        /// Show the browser UI for browser suites.
        #[arg(long)]
        headful: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum TestEnvironmentArg {
    Auto,
    Dom,
    Browser,
}

impl TestEnvironmentArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dom => "dom",
            Self::Browser => "browser",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
enum TestReporterArg {
    #[default]
    Pretty,
    Json,
    Junit,
}

impl TestReporterArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pretty => "pretty",
            Self::Json => "json",
            Self::Junit => "junit",
        }
    }
}

fn parse_test_workers(value: &str) -> Result<String, String> {
    if value == "auto" {
        return Ok(value.to_string());
    }
    if let Some(percent) = value.strip_suffix('%') {
        let percent = percent.parse::<u8>().map_err(|_| {
            "workers must be auto, a positive integer, or a percentage from 1% to 100%".to_string()
        })?;
        if (1..=100).contains(&percent) {
            return Ok(format!("{percent}%"));
        }
    } else if value.parse::<usize>().is_ok_and(|count| count > 0) {
        return Ok(value.to_string());
    }
    Err("workers must be auto, a positive integer, or a percentage from 1% to 100%".to_string())
}

fn test_worker_override(value: String) -> wake_app::WorkerOverride {
    value.parse::<usize>().map_or_else(
        |_| wake_app::WorkerOverride::Text(value),
        wake_app::WorkerOverride::Count,
    )
}

fn parse_test_shard(value: &str) -> Result<String, String> {
    let Some((index, total)) = value.split_once('/') else {
        return Err("shard must use the 1-based INDEX/TOTAL form, for example 2/3".to_string());
    };
    if total.contains('/') {
        return Err("shard must use the 1-based INDEX/TOTAL form, for example 2/3".to_string());
    }
    let index = index
        .parse::<u32>()
        .map_err(|_| "shard index must be a positive integer".to_string())?;
    let total = total
        .parse::<u32>()
        .map_err(|_| "shard total must be a positive integer".to_string())?;
    if index == 0 || total == 0 || index > total {
        return Err("shard requires 1 <= INDEX <= TOTAL".to_string());
    }
    Ok(format!("{index}/{total}"))
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

#[derive(Subcommand)]
enum LibraryCommand {
    /// Build ESM, CommonJS, declarations, and optional CSS for a component package.
    Build {
        /// Component package root.
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Library source entry relative to the package root.
        #[arg(long, default_value = "src/index.ts")]
        entry: PathBuf,
    },
    /// Generate TypeScript design tokens from token.toml.
    Token {
        /// Component package root.
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Token configuration path relative to the package root.
        #[arg(long, default_value = "token.toml")]
        config: PathBuf,
    },
    /// Generate React component documentation into public/docgen.json.
    Docgen {
        /// Component package root.
        #[arg(default_value = ".")]
        project: PathBuf,
        /// Explicit component source entry relative to the package root.
        #[arg(long)]
        entry: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum FederationCommand {
    /// Create the stable TypeScript entry used by federation type synchronization.
    Init {
        /// Project directory, or a directory below the project root.
        #[arg(default_value = ".")]
        root: PathBuf,
    },
    /// Fetch and pin the exact production manifests and asset closures.
    Lock {
        /// Project directory, or a directory below the project root.
        #[arg(default_value = ".")]
        root: PathBuf,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BundlePlatformArg {
    Browser,
    Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum BundleFormatArg {
    Iife,
    Cjs,
}

impl From<DocsModeArg> for wake_app::DocsMode {
    fn from(value: DocsModeArg) -> Self {
        match value {
            DocsModeArg::Site => Self::Site,
            DocsModeArg::Components => Self::Components,
        }
    }
}

fn selects_test_command(arguments: &[OsString]) -> bool {
    let mut index = 0;
    while let Some(argument) = arguments.get(index).and_then(|value| value.to_str()) {
        match argument {
            "--no-color" => index += 1,
            "--ui" => index += 2,
            value if value.starts_with("--ui=") => index += 1,
            value if value.starts_with('-') => return false,
            value => return value == "test",
        }
    }
    false
}

fn main() -> ExitCode {
    let raw_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.exit()
        }
        Err(error) => {
            if selects_test_command(&raw_arguments) {
                eprintln!("WAKE_TEST_CONFIG: {error}");
                return ExitCode::from(2);
            }
            error.exit()
        }
    };
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
        Command::Bundle {
            entry,
            outfile,
            platform,
            format,
            target,
            external,
            minify,
            sourcemap,
            cache,
            config,
        } => ensure_static_mode(cli.ui).and_then(|()| {
            cmd_bundle(
                BundleCommandOptions {
                    entry,
                    outfile,
                    platform,
                    format,
                    target,
                    external,
                    minify,
                    sourcemap,
                    cache,
                    config,
                },
                ui,
            )
        }),
        Command::Library { command } => {
            match command {
                LibraryCommand::Build { project, entry } => ensure_static_mode(cli.ui)
                    .and_then(|()| cmd_library_build(&project, &entry, ui)),
                LibraryCommand::Token { project, config } => ensure_static_mode(cli.ui)
                    .and_then(|()| cmd_library_token(&project, &config, ui)),
                LibraryCommand::Docgen { project, entry } => ensure_static_mode(cli.ui)
                    .and_then(|()| cmd_library_docgen(&project, entry.as_deref(), ui)),
            }
        }
        Command::Federation { command } => match command {
            FederationCommand::Init { root } => {
                ensure_static_mode(cli.ui).and_then(|()| cmd_federation_init(&root, ui))
            }
            FederationCommand::Lock { root } => {
                ensure_static_mode(cli.ui).and_then(|()| cmd_federation_lock(&root, ui))
            }
        },
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
        Command::Test {
            patterns,
            root,
            name_pattern,
            projects,
            environment,
            watch,
            changed,
            related,
            coverage,
            update_snapshots,
            serial,
            workers,
            bail,
            shard,
            seed,
            shuffle,
            reporter,
            output,
            allow_no_tests,
            browser_path,
            headful,
        } => ensure_static_mode(cli.ui).and_then(|()| {
            let selected_reporter = reporter.unwrap_or_default();
            cmd_test(
                wake_app::TestOptions {
                    root: Some(root),
                    patterns,
                    name_pattern,
                    projects,
                    environment: environment.map(|environment| environment.as_str().to_string()),
                    watch,
                    changed,
                    related,
                    coverage,
                    update_snapshots: update_snapshots.then_some("all".to_string()),
                    serial,
                    workers: workers.map(test_worker_override),
                    bail,
                    shard,
                    seed,
                    shuffle,
                    reporter: reporter.map(|reporter| reporter.as_str().to_string()),
                    output: output.clone(),
                    allow_no_tests,
                    browser_path,
                    headful,
                },
                selected_reporter,
                output.as_deref(),
                ui,
            )
        }),
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

fn cmd_federation_init(root: &Path, ui: Ui) -> Result<(), ExitCode> {
    ui.header("federation init");
    match wake_app::initialize_federation_types(root) {
        Ok(result) => {
            let status = if result.declaration == wake_app::FederationInitFileStatus::Unchanged
                && result.types_index == wake_app::FederationInitFileStatus::Unchanged
            {
                "Already initialized"
            } else {
                "Initialized"
            };
            eprintln!(
                "  {}  {status} federation types in {}",
                ui.ok("✓"),
                ui.accent(&result.project_root.display().to_string())
            );
            eprintln!();
            Ok(())
        }
        Err(error) => {
            eprintln!("wake: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_federation_lock(root: &Path, ui: Ui) -> Result<(), ExitCode> {
    ui.header("federation lock");
    let result: Result<(PathBuf, usize), String> = (|| {
        let (project_root, lock) = wake_app::generate_project_federation_lock(root)
            .map_err(|error| format!("{}: {}", error.code, error.message))?;
        Ok((project_root, lock.remotes.len()))
    })();
    match result {
        Ok((project_root, remotes)) => {
            eprintln!(
                "  {}  Locked {remotes} remote{} in {}",
                ui.ok("✓"),
                if remotes == 1 { "" } else { "s" },
                ui.accent(
                    &project_root
                        .join("wake-federation.lock")
                        .display()
                        .to_string()
                )
            );
            eprintln!();
            Ok(())
        }
        Err(error) => {
            eprintln!("wake: {error}");
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_test(
    options: wake_app::TestOptions,
    reporter: TestReporterArg,
    output: Option<&Path>,
    ui: Ui,
) -> Result<(), ExitCode> {
    if reporter == TestReporterArg::Pretty && output.is_some() {
        eprintln!("wake: --output requires --reporter json or --reporter junit");
        return Err(ExitCode::from(2));
    }
    let cancellation = wake_app::CancellationToken::default();
    let signal_cancellation = cancellation.clone();
    ctrlc::set_handler(move || signal_cancellation.cancel()).map_err(|error| {
        eprintln!("wake: could not install test interrupt handler: {error}");
        ExitCode::from(2)
    })?;
    if options.watch {
        return cmd_test_watch(options, reporter, output, ui, &cancellation);
    }
    match wake_app::run_tests(options, &cancellation) {
        Ok(result) => {
            match reporter {
                TestReporterArg::Pretty => print_pretty_test_result(&result, ui),
                TestReporterArg::Json => {
                    let serialized = serde_json::to_string(&result).map_err(|error| {
                        eprintln!("wake: could not serialize test result: {error}");
                        ExitCode::from(2)
                    })?;
                    write_test_report(&serialized, output)?;
                }
                TestReporterArg::Junit => {
                    write_test_report(&junit_test_report(&result), output)?;
                }
            }
            test_result_exit(&result)
        }
        Err(error) => Err(test_command_error_exit(ui, &error, &cancellation)),
    }
}

fn test_command_error_exit(
    ui: Ui,
    error: &wake_app::WakeError,
    cancellation: &wake_app::CancellationToken,
) -> ExitCode {
    ui.app_error(error);
    if cancellation.is_cancelled() || error.code == "WAKE_CANCELLED" {
        ExitCode::from(130)
    } else {
        ExitCode::from(2)
    }
}

fn cmd_test_watch(
    options: wake_app::TestOptions,
    reporter: TestReporterArg,
    output: Option<&Path>,
    ui: Ui,
    cancellation: &wake_app::CancellationToken,
) -> Result<(), ExitCode> {
    use crossterm::event::Event;

    let mut session = wake_app::TestSession::start(cancellation)
        .map_err(|error| test_command_error_exit(ui, &error, cancellation))?;
    session
        .start_watch(options)
        .map_err(|error| test_command_error_exit(ui, &error, cancellation))?;
    let mut last = None;
    let interactive = std::io::stdin().is_terminal();
    if interactive {
        crossterm::terminal::enable_raw_mode().map_err(|error| {
            eprintln!("wake: could not enable test watch input: {error}");
            ExitCode::from(2)
        })?;
        eprintln!(
            "Watch keys: a all · f failed · p path · t name · u snapshots · r rerun · q quit"
        );
    }
    let mut prompt: Option<(bool, String)> = None;

    let outcome = (|| {
        while !cancellation.is_cancelled() {
            session.poll_events().map_err(|error| {
                ui.app_error(&error);
                ExitCode::from(2)
            })?;
            for event in session.drain_events() {
                match event {
                    wake_app::TestSessionEvent::RunComplete { result } => {
                        if matches!(
                            result.termination_reason,
                            wake_app::TestTerminationReason::WatchRestart
                                | wake_app::TestTerminationReason::Cancelled
                        ) {
                            continue;
                        }
                        print_test_result(&result, reporter, output, ui)?;
                        last = Some(*result);
                    }
                    wake_app::TestSessionEvent::Diagnostic {
                        run_id: None,
                        diagnostic,
                    } => {
                        eprintln!("{}: {}", diagnostic.code, diagnostic.message);
                    }
                    _ => {}
                }
            }
            let input_ready = interactive
                && crossterm::event::poll(Duration::from_millis(10)).map_err(|error| {
                    eprintln!("wake: could not poll test watch input: {error}");
                    ExitCode::from(2)
                })?;
            if input_ready {
                let event = crossterm::event::read().map_err(|error| {
                    eprintln!("wake: could not read test watch input: {error}");
                    ExitCode::from(2)
                })?;
                let Event::Key(key) = event else {
                    continue;
                };
                let action = test_watch_key_action(key.code, key.modifiers);
                if action == TestWatchKeyAction::Interrupt {
                    cancellation.cancel();
                    continue;
                }
                if let Some((path_prompt, value)) = prompt.as_mut() {
                    let control = match key.code {
                        crossterm::event::KeyCode::Enter => {
                            eprintln!();
                            let value = value.trim().to_string();
                            let path_prompt = *path_prompt;
                            prompt = None;
                            (!value.is_empty()).then_some({
                                if path_prompt {
                                    wake_app::TestWatchControl::Path { pattern: value }
                                } else {
                                    wake_app::TestWatchControl::Name { pattern: value }
                                }
                            })
                        }
                        crossterm::event::KeyCode::Esc => {
                            eprintln!();
                            prompt = None;
                            None
                        }
                        crossterm::event::KeyCode::Backspace => {
                            if value.pop().is_some() {
                                eprint!("\u{8} \u{8}");
                                let _ = std::io::stderr().flush();
                            }
                            None
                        }
                        crossterm::event::KeyCode::Char(character)
                            if !key
                                .modifiers
                                .contains(crossterm::event::KeyModifiers::CONTROL) =>
                        {
                            value.push(character);
                            eprint!("{character}");
                            let _ = std::io::stderr().flush();
                            None
                        }
                        _ => None,
                    };
                    if let Some(control) = control {
                        session.watch_control(control).map_err(|error| {
                            ui.app_error(&error);
                            ExitCode::from(2)
                        })?;
                    }
                    continue;
                }
                let control = match action {
                    TestWatchKeyAction::Interrupt => {
                        unreachable!("interrupt is handled before prompt input")
                    }
                    TestWatchKeyAction::Quit => break,
                    TestWatchKeyAction::All => Some(wake_app::TestWatchControl::All),
                    TestWatchKeyAction::Failed => Some(wake_app::TestWatchControl::Failed),
                    TestWatchKeyAction::UpdateSnapshots => {
                        Some(wake_app::TestWatchControl::UpdateSnapshots)
                    }
                    TestWatchKeyAction::Rerun => Some(wake_app::TestWatchControl::Rerun),
                    TestWatchKeyAction::PromptPath => {
                        eprint!("\nPath pattern: ");
                        let _ = std::io::stderr().flush();
                        prompt = Some((true, String::new()));
                        None
                    }
                    TestWatchKeyAction::PromptName => {
                        eprint!("\nTest name pattern: ");
                        let _ = std::io::stderr().flush();
                        prompt = Some((false, String::new()));
                        None
                    }
                    TestWatchKeyAction::Ignore => None,
                };
                if let Some(control) = control {
                    session.watch_control(control).map_err(|error| {
                        ui.app_error(&error);
                        ExitCode::from(2)
                    })?;
                }
            } else if !interactive {
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        if cancellation.is_cancelled() {
            Err(ExitCode::from(130))
        } else if let Some(last) = &last {
            test_result_exit(last)
        } else {
            Ok(())
        }
    })();

    if interactive {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    if let Err(error) = session.close() {
        ui.app_error(&error);
        return Err(ExitCode::from(2));
    }
    outcome
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestWatchKeyAction {
    All,
    Failed,
    PromptPath,
    PromptName,
    UpdateSnapshots,
    Rerun,
    Quit,
    Interrupt,
    Ignore,
}

fn test_watch_key_action(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> TestWatchKeyAction {
    use crossterm::event::{KeyCode, KeyModifiers};

    match (code, modifiers) {
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            TestWatchKeyAction::Interrupt
        }
        (KeyCode::Char('q'), _) => TestWatchKeyAction::Quit,
        (KeyCode::Char('a'), _) => TestWatchKeyAction::All,
        (KeyCode::Char('f'), _) => TestWatchKeyAction::Failed,
        (KeyCode::Char('p'), _) => TestWatchKeyAction::PromptPath,
        (KeyCode::Char('t'), _) => TestWatchKeyAction::PromptName,
        (KeyCode::Char('u'), _) => TestWatchKeyAction::UpdateSnapshots,
        (KeyCode::Char('r'), _) => TestWatchKeyAction::Rerun,
        _ => TestWatchKeyAction::Ignore,
    }
}

fn print_test_result(
    result: &wake_app::TestRunResult,
    reporter: TestReporterArg,
    output: Option<&Path>,
    ui: Ui,
) -> Result<(), ExitCode> {
    match reporter {
        TestReporterArg::Pretty => print_pretty_test_result(result, ui),
        TestReporterArg::Json => {
            let serialized = serde_json::to_string(result).map_err(|error| {
                eprintln!("wake: could not serialize test result: {error}");
                ExitCode::from(2)
            })?;
            write_test_report(&serialized, output)?;
        }
        TestReporterArg::Junit => write_test_report(&junit_test_report(result), output)?,
    }
    Ok(())
}

fn test_result_exit(result: &wake_app::TestRunResult) -> Result<(), ExitCode> {
    match result.termination_reason {
        wake_app::TestTerminationReason::Cancelled
        | wake_app::TestTerminationReason::WatchRestart => Err(ExitCode::from(130)),
        wake_app::TestTerminationReason::HostCrash
        | wake_app::TestTerminationReason::Oom
        | wake_app::TestTerminationReason::InternalError => Err(ExitCode::from(2)),
        wake_app::TestTerminationReason::Completed
        | wake_app::TestTerminationReason::Bail
        | wake_app::TestTerminationReason::Timeout => {
            if result.success {
                Ok(())
            } else {
                Err(ExitCode::FAILURE)
            }
        }
    }
}

fn print_pretty_test_result(result: &wake_app::TestRunResult, ui: Ui) {
    for suite in &result.suites {
        let status = match suite.status {
            wake_app::TestSuiteStatus::Passed => ui.ok("PASS"),
            wake_app::TestSuiteStatus::Failed => ui.err("FAIL"),
            wake_app::TestSuiteStatus::Skipped => ui.dim("SKIP"),
        };
        eprintln!("{status} {}", suite.path);
        for test in &suite.tests {
            let marker = match test.status {
                wake_app::TestStatus::Passed => ui.ok("✓"),
                wake_app::TestStatus::Failed => ui.err("✕"),
                wake_app::TestStatus::Skipped => ui.dim("○"),
                wake_app::TestStatus::Todo => ui.dim("✎"),
            };
            eprintln!("  {marker} {}", test.name);
            for failure in &test.failures {
                let failure = format_test_failure(failure);
                eprintln!("    {}", failure.replace('\n', "\n    "));
            }
        }
        for failure in &suite.failures {
            let failure = format_test_failure(failure);
            eprintln!("  {}", failure.replace('\n', "\n  "));
        }
    }
    eprintln!(
        "Test Suites: {} passed, {} failed, {} total",
        result.counts.suites.passed, result.counts.suites.failed, result.counts.suites.total
    );
    eprintln!(
        "Tests:       {} passed, {} failed, {} pending, {} total",
        result.counts.tests.passed,
        result.counts.tests.failed,
        result.counts.tests.skipped + result.counts.tests.todo,
        result.counts.tests.total
    );
    if let Some(artifact) = result
        .artifacts
        .iter()
        .find(|artifact| artifact.kind == "coverage-text")
    {
        match std::fs::read_to_string(&artifact.path) {
            Ok(report) => eprintln!("{}", report.trim_end()),
            Err(error) => eprintln!(
                "WAKE_TEST_COVERAGE: could not read text report {}: {error}",
                artifact.path
            ),
        }
    }
    eprintln!("Seed:        {}", result.seed);
    eprintln!("Time:        {} ms", result.duration_ms);
    if result.termination_reason != wake_app::TestTerminationReason::Completed {
        eprintln!("Termination: {:?}", result.termination_reason);
    }
    for diagnostic in &result.diagnostics {
        eprintln!("{}: {}", diagnostic.code, diagnostic.message);
    }
}

fn format_test_failure(failure: &wake_app::TestFailure) -> String {
    let mut rendered = String::new();
    if let Some(code) = failure.code.as_deref() {
        rendered.push_str(code);
        rendered.push_str(": ");
    }
    rendered.push_str(&failure.message);
    if let Some(location) = &failure.location {
        rendered.push_str(&format!(
            "\n  at {}:{}:{}",
            location.path, location.line, location.column
        ));
    }
    if let Some(unified) = failure
        .diff
        .as_ref()
        .and_then(|diff| diff.unified.as_deref())
        .filter(|diff| !diff.is_empty())
    {
        rendered.push('\n');
        rendered.push_str(unified);
    }
    if let Some(stack) = failure
        .stack
        .as_deref()
        .filter(|stack| !stack.is_empty() && !rendered.contains(stack))
    {
        rendered.push('\n');
        rendered.push_str(stack);
    }
    rendered
}

fn format_test_failures(failures: &[wake_app::TestFailure]) -> String {
    failures
        .iter()
        .map(format_test_failure)
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_test_report(report: &str, output: Option<&Path>) -> Result<(), ExitCode> {
    if let Some(output) = output {
        std::fs::write(output, report).map_err(|error| {
            eprintln!(
                "wake: could not write test report {}: {error}",
                output.display()
            );
            ExitCode::from(2)
        })
    } else {
        println!("{report}");
        Ok(())
    }
}

fn junit_test_report(result: &wake_app::TestRunResult) -> String {
    let suite_errors = result
        .suites
        .iter()
        .filter(|suite| !suite.failures.is_empty())
        .count();
    let mut report = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{:.3}\">\n",
        result.counts.tests.total + suite_errors,
        result.counts.tests.failed,
        suite_errors,
        result.counts.tests.skipped + result.counts.tests.todo,
        result.duration_ms as f64 / 1_000.0
    );
    for suite in &result.suites {
        let failures = suite
            .tests
            .iter()
            .filter(|test| test.status == wake_app::TestStatus::Failed)
            .count();
        let skipped = suite
            .tests
            .iter()
            .filter(|test| {
                matches!(
                    test.status,
                    wake_app::TestStatus::Skipped | wake_app::TestStatus::Todo
                )
            })
            .count();
        let suite_error = usize::from(!suite.failures.is_empty());
        report.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" errors=\"{}\" skipped=\"{}\" time=\"{:.3}\">\n",
            xml_escape(&suite.path),
            suite.tests.len() + suite_error,
            failures,
            suite_error,
            skipped,
            suite.duration_ms as f64 / 1_000.0
        ));
        for test in &suite.tests {
            report.push_str(&format!(
                "    <testcase name=\"{}\" classname=\"{}\" time=\"{:.3}\">",
                xml_escape(&test.name),
                xml_escape(&suite.path),
                test.duration_ms as f64 / 1_000.0
            ));
            match test.status {
                wake_app::TestStatus::Passed => {}
                wake_app::TestStatus::Failed => {
                    report.push_str("<failure>");
                    report.push_str(&xml_escape(&format_test_failures(&test.failures)));
                    report.push_str("</failure>");
                }
                wake_app::TestStatus::Skipped | wake_app::TestStatus::Todo => {
                    report.push_str("<skipped />");
                }
            }
            report.push_str("</testcase>\n");
        }
        if !suite.failures.is_empty() {
            report.push_str("    <testcase name=\"[suite setup]\"><error>");
            report.push_str(&xml_escape(&format_test_failures(&suite.failures)));
            report.push_str("</error></testcase>\n");
        }
        report.push_str("  </testsuite>\n");
    }
    report.push_str("</testsuites>");
    report
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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
            .filter(|file| file.kind == wake_app::OutputFileKind::Chunk)
            .count(),
        assets: result
            .files
            .iter()
            .filter(|file| {
                matches!(
                    file.kind,
                    wake_app::OutputFileKind::Asset | wake_app::OutputFileKind::Css
                )
            })
            .count(),
        duration_ms: result.duration_ms,
    }
}

struct BundleCommandOptions {
    entry: PathBuf,
    outfile: PathBuf,
    platform: Option<BundlePlatformArg>,
    format: Option<BundleFormatArg>,
    target: Option<String>,
    external: Vec<String>,
    minify: bool,
    sourcemap: bool,
    cache: bool,
    config: Option<PathBuf>,
}

fn cmd_bundle(options: BundleCommandOptions, ui: Ui) -> Result<(), ExitCode> {
    ui.header("bundle");
    let platform = options.platform.map(|platform| match platform {
        BundlePlatformArg::Browser => wake_app::BuildPlatform::Browser,
        BundlePlatformArg::Node => wake_app::BuildPlatform::Node,
    });
    let format = options.format.map(|format| match format {
        BundleFormatArg::Iife => wake_app::ModuleFormat::Iife,
        BundleFormatArg::Cjs => wake_app::ModuleFormat::CommonJs,
    });
    let project = wake_app::ProjectOptions {
        cwd: std::env::current_dir().ok(),
        config_path: options.config,
    };
    match wake_app::bundle(
        wake_app::BundleOptions {
            project,
            entry: Some(options.entry),
            outfile: Some(options.outfile),
            platform,
            format,
            target: options.target,
            external: options.external,
            minify: options.minify,
            source_map: options.sourcemap,
            cache: options.cache,
        },
        &wake_app::CancellationToken::default(),
    ) {
        Ok(result) => {
            ui.bundle_result("Bundled", &result, None);
            Ok(())
        }
        Err(error) => {
            ui.app_error(&error);
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_library_token(project: &Path, config: &Path, ui: Ui) -> Result<(), ExitCode> {
    ui.header("library token");
    match wake_app::generate_css_token(
        wake_app::GenerateCssTokenOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(project.to_path_buf()),
                config_path: None,
            },
            config_path: Some(config.to_path_buf()),
        },
        &wake_app::CancellationToken::default(),
    ) {
        Ok(result) => {
            eprintln!(
                "  {}  Generated {}",
                ui.ok("✓"),
                ui.accent(&result.output_file)
            );
            eprintln!();
            Ok(())
        }
        Err(error) => {
            ui.app_error(&error);
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_library_build(project: &Path, entry: &Path, ui: Ui) -> Result<(), ExitCode> {
    ui.header("library build");
    match wake_app::build_library(
        wake_app::LibraryBuildOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(project.to_path_buf()),
                config_path: None,
            },
            entry: Some(entry.to_path_buf()),
        },
        &wake_app::CancellationToken::default(),
    ) {
        Ok(result) => {
            eprintln!(
                "  {}  Built {} files from {} modules in {:.1}ms",
                ui.ok("✓"),
                ui.accent(&result.files.len().to_string()),
                ui.accent(&result.module_count.to_string()),
                result.duration_ms
            );
            eprintln!();
            Ok(())
        }
        Err(error) => {
            ui.app_error(&error);
            Err(ExitCode::FAILURE)
        }
    }
}

fn cmd_library_docgen(project: &Path, entry: Option<&Path>, ui: Ui) -> Result<(), ExitCode> {
    ui.header("library docgen");
    match wake_app::generate_docgen(
        wake_app::GenerateDocgenOptions {
            project: wake_app::ProjectOptions {
                cwd: Some(project.to_path_buf()),
                config_path: None,
            },
            entry: entry.map(Path::to_path_buf),
        },
        &wake_app::CancellationToken::default(),
    ) {
        Ok(result) => {
            eprintln!(
                "  {}  Generated {}",
                ui.ok("✓"),
                ui.accent(&result.output_file)
            );
            eprintln!();
            Ok(())
        }
        Err(error) => {
            ui.app_error(&error);
            Err(ExitCode::FAILURE)
        }
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
        federation: None,
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

enum BuildWatchNotification {
    Paths(Vec<(PathBuf, bool)>),
    Rescan,
    BackendError { generation: u64, error: String },
}

fn create_build_watcher(
    sender: std::sync::mpsc::Sender<BuildWatchNotification>,
    failed_generation: Arc<AtomicU64>,
    generation: u64,
    cancellation: wake_app::CancellationToken,
) -> Result<notify::RecommendedWatcher, String> {
    notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
        Ok(event) if event.need_rescan() => {
            let _ = sender.send(BuildWatchNotification::Rescan);
        }
        Ok(event) if is_build_watch_event(&event) => {
            let structural = is_build_watch_structural_event(&event);
            let _ = sender.send(BuildWatchNotification::Paths(
                event
                    .paths
                    .into_iter()
                    .map(|path| (path, structural))
                    .collect(),
            ));
        }
        Ok(_) => {}
        Err(error) => {
            // Invalidate the coverage capability before queueing the diagnostic. The command
            // loop may already have a rebuild pending, so queue ordering alone is not a fence.
            revoke_build_watch_backend(&failed_generation, generation, &cancellation);
            let _ = sender.send(BuildWatchNotification::BackendError {
                generation,
                error: format!("file watcher backend error: {error}"),
            });
        }
    })
    .map_err(|error| format!("cannot create file watcher: {error}"))
}

fn revoke_build_watch_backend(
    failed_generation: &AtomicU64,
    generation: u64,
    cancellation: &wake_app::CancellationToken,
) {
    // Cancellation is generation-owned. A delayed callback from a retired watcher can revoke its
    // own in-flight build, but cannot cancel work authorized by the successor generation.
    revoke_build_watch_backend_with(failed_generation, generation, || cancellation.cancel());
}

fn revoke_build_watch_backend_with(
    failed_generation: &AtomicU64,
    generation: u64,
    cancel: impl FnOnce(),
) {
    // Publish the capability revocation before cancel() waits for any in-flight output commit.
    // This keeps a second queued rebuild from observing a live epoch during that wait.
    failed_generation.fetch_max(generation, Ordering::Release);
    cancel();
}

fn report_build_watch_failure(
    state: &mut DashboardState,
    dashboard: &Option<Dashboard>,
    ui: &Ui,
    error: &str,
) {
    state.error(format!("[WAKE_WATCH] {error}"));
    if dashboard.is_none() {
        eprintln!("  {}  [WAKE_WATCH] {error}", ui.err("✗"));
    }
}

fn is_current_build_watch_backend_error(
    active_generation: Option<u64>,
    event_generation: u64,
) -> bool {
    active_generation == Some(event_generation)
}

#[derive(Default)]
struct BuildWatchCoverageCapability {
    installed: Option<(wake_app::WatchPlanSnapshot, u64)>,
}

impl BuildWatchCoverageCapability {
    fn confirm(&mut self, plan: wake_app::WatchPlanSnapshot, backend_generation: u64) -> bool {
        let next = (plan, backend_generation);
        let changed = self.installed.as_ref() != Some(&next);
        self.installed = Some(next);
        changed
    }

    fn clear(&mut self) {
        self.installed = None;
    }

    fn installed(
        &self,
        backend_generation: u64,
        failed_generation: &AtomicU64,
    ) -> Option<&wake_app::WatchPlanSnapshot> {
        if failed_generation.load(Ordering::Acquire) >= backend_generation {
            return None;
        }
        self.installed
            .as_ref()
            .filter(|(_, generation)| *generation == backend_generation)
            .map(|(plan, _)| plan)
    }

    fn revision_for(
        &self,
        required: &wake_app::WatchPlanSnapshot,
        backend_generation: u64,
        failed_generation: &AtomicU64,
    ) -> Option<wake_app::WatchPlanRevision> {
        let installed = self.installed(backend_generation, failed_generation)?;
        (installed.root == required.root
            && installed.revision == required.revision
            && required
                .interests
                .iter()
                .all(|interest| installed.interests.contains(interest)))
        .then_some(required.revision)
    }

    fn narrow_to(
        &mut self,
        plan: wake_app::WatchPlanSnapshot,
        backend_generation: u64,
        failed_generation: &AtomicU64,
    ) -> bool {
        let Some(installed) = self.installed(backend_generation, failed_generation) else {
            return false;
        };
        if installed.root != plan.root
            || plan.revision < installed.revision
            || !plan
                .interests
                .iter()
                .all(|interest| installed.interests.contains(interest))
        {
            return false;
        }
        self.installed = Some((plan, backend_generation));
        true
    }
}

fn build_watch_handoff_plan(
    bootstrap: &wake_app::WatchPlanSnapshot,
    context: &wake_app::WatchPlanSnapshot,
) -> wake_app::WatchPlanSnapshot {
    let mut interests = bootstrap
        .interests
        .iter()
        .chain(&context.interests)
        .cloned()
        .collect::<Vec<_>>();
    interests.sort();
    interests.dedup();
    wake_app::WatchPlanSnapshot {
        revision: context.revision,
        root: context.root.clone(),
        interests,
    }
}

fn current_build_watch_plan(
    bootstrap: Option<&wake_app::BuildWatchBootstrap>,
    context: Option<&wake_app::BuildContext>,
    handoff: Option<&wake_app::WatchPlanSnapshot>,
) -> wake_app::WatchPlanSnapshot {
    if let Some(context) = context {
        let context = context.watch_plan();
        return handoff.map_or_else(
            || context.clone(),
            |bootstrap| build_watch_handoff_plan(bootstrap, &context),
        );
    }
    bootstrap
        .expect("a build watch without a context owns a bootstrap")
        .watch_plan()
}

fn report_build_watch_app_error(
    state: &mut DashboardState,
    dashboard: &Option<Dashboard>,
    ui: &Ui,
    error: &wake_app::WakeError,
) {
    if error.diagnostics.is_empty() {
        state.error(format!("[{}] {}", error.code, error.message));
    } else {
        for diagnostic in &error.diagnostics {
            state.diagnostic(diagnostic.clone(), format_diagnostic_plain(diagnostic));
        }
    }
    if dashboard.is_none() {
        ui.app_error(error);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildWatchRebuildOutcome {
    Succeeded,
    Failed,
    BackendLost,
    CoveragePending,
    RestartRequired,
}

fn execute_build_watch_rebuild(
    context: &wake_app::BuildContext,
    invalidation: wake_app::WatchInvalidation,
    covered_revision: wake_app::WatchPlanRevision,
    cancellation: wake_app::CancellationToken,
    initial: bool,
    state: &mut DashboardState,
    dashboard: &mut Option<Dashboard>,
    ui: &Ui,
) -> BuildWatchRebuildOutcome {
    let changed = match &invalidation {
        wake_app::WatchInvalidation::Paths(paths) => paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        wake_app::WatchInvalidation::Rescan => vec!["<filesystem rescan>".to_owned()],
    };
    state.rebuilding(changed.clone(), None, None);
    if let Some(active) = dashboard.as_mut() {
        let _ = active.draw(state);
    } else {
        ui.rebuild_start(changed.len());
    }

    match context.rebuild_watch_at(invalidation, covered_revision, cancellation.clone()) {
        Ok(result) => {
            let metrics = metrics_from_result(&result);
            state.built(metrics, initial, None, None);
            if dashboard.is_some() {
                record_successful_build_diagnostics(state, &result.diagnostics);
            } else if initial {
                ui.build_result("Initial build completed", &result, None);
                eprintln!(
                    "     {}  {}",
                    ui.dim("Watching"),
                    ui.accent("project source and exact control inputs")
                );
                eprintln!();
            } else {
                ui.rebuilt(metrics, false);
                for diagnostic in &result.diagnostics {
                    ui.diagnostic(diagnostic);
                }
            }
            BuildWatchRebuildOutcome::Succeeded
        }
        Err(error) if error.code == "WAKE_WATCH_COVERAGE_PENDING" => {
            BuildWatchRebuildOutcome::CoveragePending
        }
        Err(error) if error.code == "WAKE_DEV_RESTART_REQUIRED" => {
            BuildWatchRebuildOutcome::RestartRequired
        }
        Err(error) if error.code == "WAKE_CANCELLED" && cancellation.is_cancelled() => {
            BuildWatchRebuildOutcome::BackendLost
        }
        Err(error) => {
            report_build_watch_app_error(state, dashboard, ui, &error);
            BuildWatchRebuildOutcome::Failed
        }
    }
}

fn next_watch_retry_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(Duration::from_secs(2))
}

fn build_watch_reconcile_needs_rescan(
    newly_installed: bool,
    coverage_changed: bool,
    cleanup_only: bool,
) -> bool {
    newly_installed || (coverage_changed && !cleanup_only)
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

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut state = DashboardState::new(
        "build --watch",
        &cwd,
        "WATCH",
        format!("typed project inputs · writing {}", outdir.display()),
    );
    state.set_endpoint("typed project inputs".to_string());
    let mut dashboard = start_dashboard(mode, &ui, &state)?;

    let build_options = wake_app::BuildOptions {
        project: wake_app::ProjectOptions {
            cwd: Some(cwd.clone()),
            config_path: None,
        },
        entry: entry.map(Path::to_path_buf),
        outdir: Some(outdir.to_path_buf()),
        cache,
        source_map: sourcemap,
        write: true,
        federation: None,
    };
    let mut bootstrap = match wake_app::BuildWatchBootstrap::create(build_options.clone()) {
        Ok(bootstrap) => Some(bootstrap),
        Err(error) => {
            return Err(restore_for_error(
                &mut dashboard,
                &ui,
                "build --watch",
                &error,
            ));
        }
    };
    if let wake_app::BuildWatchBootstrapState::Waiting { error, .. } = bootstrap
        .as_ref()
        .expect("build watch bootstrap exists")
        .state()
    {
        report_build_watch_app_error(&mut state, &dashboard, &ui, &error);
    }
    let mut context: Option<wake_app::BuildContext> = None;
    let mut handoff_plan: Option<wake_app::WatchPlanSnapshot> = None;

    // The bootstrap is probe-only. Its complete recovery plan is installed before candidate
    // generation or retained-session creation; a mandatory Rescan then revalidates that snapshot.
    let (tx, rx) = mpsc::channel::<BuildWatchNotification>();
    let failed_backend_generation = Arc::new(AtomicU64::new(0));
    let mut next_backend_generation = 1_u64;
    let mut last_watch_error = None;
    let initial_backend_generation = next_backend_generation;
    next_backend_generation = next_backend_generation.saturating_add(1);
    let initial_backend_cancellation = wake_app::CancellationToken::default();
    let (mut watcher, mut watcher_generation, mut watcher_cancellation) = match create_build_watcher(
        tx.clone(),
        Arc::clone(&failed_backend_generation),
        initial_backend_generation,
        initial_backend_cancellation.clone(),
    ) {
        Ok(watcher) => (
            Some(watcher),
            Some(initial_backend_generation),
            Some(initial_backend_cancellation),
        ),
        Err(error) => {
            report_build_watch_failure(&mut state, &dashboard, &ui, &error);
            last_watch_error = Some(error);
            (None, None, None)
        }
    };
    let mut registrations = wake_app::WatchRegistrationState::default();
    let mut desired_plan = bootstrap
        .as_ref()
        .expect("build watch bootstrap exists")
        .watch_plan();
    let mut coverage = BuildWatchCoverageCapability::default();
    let mut pending_invalidation: Option<wake_app::WatchInvalidation> = None;
    let mut retry_at = Some(Instant::now());
    let mut retry_delay = Duration::from_millis(100);
    let mut recreate_backend = watcher.is_none();
    let mut published_once = false;
    let mut cleanup_only_reconcile = false;
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
                context.as_ref(),
                &mut dashboard,
                &mut state,
                &ui,
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
                        context.as_ref(),
                        &mut dashboard,
                        &mut state,
                        &ui,
                        "q",
                        None,
                    );
                }
                DashboardAction::Interrupt => {
                    return finish_watch(
                        context.as_ref(),
                        &mut dashboard,
                        &mut state,
                        &ui,
                        "Ctrl-C",
                        Some(130),
                    );
                }
                DashboardAction::Continue => {}
            }
        }

        // A watcher callback invalidates its generation before it queues the error diagnostic.
        // Observe that fence before registration, activation, or a retained-context rebuild so a
        // queued backend failure can never authorize publication through stale coverage.
        if watcher_generation.is_some_and(|generation| {
            failed_backend_generation.load(Ordering::Acquire) >= generation
        }) {
            watcher.take();
            watcher_generation = None;
            if let Some(cancellation) = watcher_cancellation.take() {
                cancellation.cancel();
            }
            registrations.clear_after_backend_loss();
            coverage.clear();
            cleanup_only_reconcile = false;
            pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
            recreate_backend = true;
            retry_at = Some(Instant::now());
            continue;
        }

        if retry_at.is_some_and(|retry| Instant::now() >= retry) {
            if recreate_backend {
                let backend_generation = next_backend_generation;
                next_backend_generation = next_backend_generation.saturating_add(1);
                let backend_cancellation = wake_app::CancellationToken::default();
                match create_build_watcher(
                    tx.clone(),
                    Arc::clone(&failed_backend_generation),
                    backend_generation,
                    backend_cancellation.clone(),
                ) {
                    Ok(created) => {
                        watcher = Some(created);
                        watcher_generation = Some(backend_generation);
                        watcher_cancellation = Some(backend_cancellation);
                        registrations.clear_after_backend_loss();
                        coverage.clear();
                        cleanup_only_reconcile = false;
                        recreate_backend = false;
                    }
                    Err(error) => {
                        if last_watch_error.as_deref() != Some(error.as_str()) {
                            report_build_watch_failure(&mut state, &dashboard, &ui, &error);
                            last_watch_error = Some(error);
                        }
                        retry_at = Some(Instant::now() + retry_delay);
                        retry_delay = next_watch_retry_delay(retry_delay);
                        continue;
                    }
                }
            }

            desired_plan = current_build_watch_plan(
                bootstrap.as_ref(),
                context.as_ref(),
                handoff_plan.as_ref(),
            );
            let Some(active_watcher) = watcher.as_mut() else {
                coverage.clear();
                recreate_backend = true;
                retry_at = Some(Instant::now() + retry_delay);
                retry_delay = next_watch_retry_delay(retry_delay);
                continue;
            };
            match wake_app::reconcile_watch_interests(
                active_watcher,
                &mut registrations,
                &desired_plan.interests,
            ) {
                Ok(outcome) => {
                    for error in &outcome.cleanup_errors {
                        if last_watch_error.as_deref() != Some(error.as_str()) {
                            report_build_watch_failure(&mut state, &dashboard, &ui, error);
                            last_watch_error = Some(error.clone());
                        }
                    }
                    if !outcome.cleanup_errors.is_empty() {
                        watcher.take();
                        watcher_generation = None;
                        if let Some(cancellation) = watcher_cancellation.take() {
                            cancellation.cancel();
                        }
                        registrations.clear_after_backend_loss();
                        coverage.clear();
                        cleanup_only_reconcile = false;
                        pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                        recreate_backend = true;
                        retry_at = Some(Instant::now() + retry_delay);
                        retry_delay = next_watch_retry_delay(retry_delay);
                        continue;
                    }
                    if registrations.is_coverage_complete(&desired_plan.interests) {
                        let newly_installed = coverage.confirm(
                            desired_plan.clone(),
                            watcher_generation.expect("a reconciled watcher has a generation"),
                        );
                        if build_watch_reconcile_needs_rescan(
                            newly_installed,
                            outcome.coverage_changed,
                            cleanup_only_reconcile,
                        ) {
                            pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                        }
                    } else {
                        coverage.clear();
                        cleanup_only_reconcile = false;
                    }
                    if outcome.cleanup_errors.is_empty()
                        && registrations.is_converged(&desired_plan.interests)
                    {
                        retry_at = None;
                        retry_delay = Duration::from_millis(100);
                        last_watch_error = None;
                        cleanup_only_reconcile = false;
                    } else {
                        retry_at = Some(Instant::now() + retry_delay);
                        retry_delay = next_watch_retry_delay(retry_delay);
                    }
                }
                Err(error) => {
                    watcher.take();
                    watcher_generation = None;
                    if let Some(cancellation) = watcher_cancellation.take() {
                        cancellation.cancel();
                    }
                    registrations.clear_after_backend_loss();
                    coverage.clear();
                    cleanup_only_reconcile = false;
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    recreate_backend = true;
                    let error = error.to_string();
                    if last_watch_error.as_deref() != Some(error.as_str()) {
                        report_build_watch_failure(&mut state, &dashboard, &ui, &error);
                        last_watch_error = Some(error);
                    }
                    retry_at = Some(Instant::now() + retry_delay);
                    retry_delay = next_watch_retry_delay(retry_delay);
                }
            }
        }

        if let Some(invalidation) = pending_invalidation.take() {
            if !registrations.is_coverage_complete(&desired_plan.interests) {
                pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                retry_at = Some(Instant::now());
                continue;
            }

            if context.is_none() {
                let required = bootstrap
                    .as_ref()
                    .expect("a watch without a context owns a bootstrap")
                    .watch_plan();
                let Some(active_generation) = watcher_generation else {
                    coverage.clear();
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    retry_at = Some(Instant::now());
                    continue;
                };
                let Some(covered_revision) =
                    coverage.revision_for(&required, active_generation, &failed_backend_generation)
                else {
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    retry_at = Some(Instant::now());
                    continue;
                };
                if failed_backend_generation.load(Ordering::Acquire) >= active_generation {
                    coverage.clear();
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    retry_at = Some(Instant::now());
                    continue;
                }
                let activation = bootstrap
                    .as_mut()
                    .expect("a watch without a context owns a bootstrap")
                    .activate_at(covered_revision);
                match activation {
                    Ok(activated) => {
                        handoff_plan = Some(
                            bootstrap
                                .as_ref()
                                .expect("activated bootstrap remains available for handoff")
                                .watch_plan(),
                        );
                        context = Some(activated);
                        bootstrap = None;
                        desired_plan =
                            current_build_watch_plan(None, context.as_ref(), handoff_plan.as_ref());
                        coverage.clear();
                        pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                        retry_at = Some(Instant::now());
                    }
                    Err(error) if error.code == "WAKE_WATCH_COVERAGE_PENDING" => {
                        desired_plan = bootstrap
                            .as_ref()
                            .expect("coverage-pending bootstrap remains active")
                            .watch_plan();
                        coverage.clear();
                        pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                        retry_at = Some(Instant::now());
                    }
                    Err(error) => {
                        report_build_watch_app_error(&mut state, &dashboard, &ui, &error);
                        let latest = bootstrap
                            .as_ref()
                            .expect("waiting bootstrap remains active")
                            .watch_plan();
                        if latest != desired_plan {
                            desired_plan = latest;
                            coverage.clear();
                            pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                            retry_at = Some(Instant::now());
                        }
                    }
                }
                continue;
            }

            let required = context
                .as_ref()
                .expect("active build watch context")
                .watch_plan();
            let Some(active_generation) = watcher_generation else {
                coverage.clear();
                pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                retry_at = Some(Instant::now());
                continue;
            };
            let Some(covered_revision) =
                coverage.revision_for(&required, active_generation, &failed_backend_generation)
            else {
                coverage.clear();
                pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                retry_at = Some(Instant::now());
                continue;
            };
            if failed_backend_generation.load(Ordering::Acquire) >= active_generation {
                coverage.clear();
                pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                retry_at = Some(Instant::now());
                continue;
            }
            let Some(build_cancellation) = watcher_cancellation.clone() else {
                coverage.clear();
                pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                retry_at = Some(Instant::now());
                continue;
            };
            let outcome = execute_build_watch_rebuild(
                context.as_ref().expect("active build watch context"),
                invalidation,
                covered_revision,
                build_cancellation,
                !published_once,
                &mut state,
                &mut dashboard,
                &ui,
            );
            let mut shrink_only = false;
            match outcome {
                BuildWatchRebuildOutcome::Succeeded => {
                    published_once = true;
                    handoff_plan = None;
                    shrink_only = true;
                }
                BuildWatchRebuildOutcome::CoveragePending => {
                    coverage.clear();
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    retry_at = Some(Instant::now());
                }
                BuildWatchRebuildOutcome::BackendLost => {
                    watcher.take();
                    watcher_generation = None;
                    if let Some(cancellation) = watcher_cancellation.take() {
                        cancellation.cancel();
                    }
                    registrations.clear_after_backend_loss();
                    coverage.clear();
                    cleanup_only_reconcile = false;
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    recreate_backend = true;
                    retry_delay = Duration::from_millis(100);
                    retry_at = Some(Instant::now() + retry_delay);
                }
                BuildWatchRebuildOutcome::RestartRequired => {
                    if let Some(active) = context.take() {
                        active.close();
                    }
                    handoff_plan = None;
                    match wake_app::BuildWatchBootstrap::create(build_options.clone()) {
                        Ok(next) => {
                            if let wake_app::BuildWatchBootstrapState::Waiting { error, .. } =
                                next.state()
                            {
                                report_build_watch_app_error(&mut state, &dashboard, &ui, &error);
                            }
                            bootstrap = Some(next);
                            coverage.clear();
                            pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                            retry_at = Some(Instant::now());
                        }
                        Err(error) => {
                            return Err(restore_for_error(
                                &mut dashboard,
                                &ui,
                                "build --watch",
                                &error,
                            ));
                        }
                    }
                }
                BuildWatchRebuildOutcome::Failed => {}
            }

            let latest = current_build_watch_plan(
                bootstrap.as_ref(),
                context.as_ref(),
                handoff_plan.as_ref(),
            );
            if latest != desired_plan {
                // The bootstrap/context union remains installed through the first successful
                // publication. Afterwards reconciliation can safely shrink to context ownership.
                desired_plan = latest.clone();
                if shrink_only
                    && watcher_generation.is_some_and(|generation| {
                        coverage.narrow_to(latest, generation, &failed_backend_generation)
                    })
                {
                    // Removing bootstrap-only registrations creates no observation gap. Reconcile
                    // cleanup without scheduling a second initial build.
                    cleanup_only_reconcile = true;
                    retry_at = Some(Instant::now());
                } else {
                    coverage.clear();
                    pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
                    retry_at = Some(Instant::now());
                }
            }
            continue;
        }

        let next = if dashboard.is_some() {
            rx.try_recv().ok()
        } else {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(event) => Some(event),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return finish_watch(
                        context.as_ref(),
                        &mut dashboard,
                        &mut state,
                        &ui,
                        "watcher closed",
                        None,
                    );
                }
            }
        };
        let Some(next) = next else {
            continue;
        };

        let mut changed_events = Vec::new();
        let mut needs_rescan = false;
        let mut backend_error = None;
        match next {
            BuildWatchNotification::Paths(events) => changed_events.extend(events),
            BuildWatchNotification::Rescan => needs_rescan = true,
            BuildWatchNotification::BackendError { generation, error } => {
                backend_error = Some((generation, error));
            }
        }
        std::thread::sleep(Duration::from_millis(30));
        while let Ok(event) = rx.try_recv() {
            match event {
                BuildWatchNotification::Paths(events) => changed_events.extend(events),
                BuildWatchNotification::Rescan => needs_rescan = true,
                BuildWatchNotification::BackendError { generation, error } => {
                    backend_error = Some((generation, error));
                }
            }
        }
        // The atomic fence may already have retired a generation and successfully installed its
        // successor. A delayed diagnostic from that old backend is stale state, not a new failure
        // of the recovered watcher.
        if let Some((generation, error)) = backend_error
            && is_current_build_watch_backend_error(watcher_generation, generation)
        {
            if last_watch_error.as_deref() != Some(error.as_str()) {
                report_build_watch_failure(&mut state, &dashboard, &ui, &error);
                last_watch_error = Some(error);
            }
            watcher.take();
            watcher_generation = None;
            if let Some(cancellation) = watcher_cancellation.take() {
                cancellation.cancel();
            }
            registrations.clear_after_backend_loss();
            coverage.clear();
            cleanup_only_reconcile = false;
            pending_invalidation = Some(wake_app::WatchInvalidation::Rescan);
            recreate_backend = true;
            retry_delay = Duration::from_millis(100);
            retry_at = Some(Instant::now() + retry_delay);
            continue;
        }

        let Some(active_generation) = watcher_generation else {
            continue;
        };
        let Some(installed) = coverage.installed(active_generation, &failed_backend_generation)
        else {
            // Coverage is incomplete. A successful timed reconciliation always follows with a
            // Rescan, so events observed through partial coverage do not become false evidence.
            continue;
        };
        let mut changed = Vec::new();
        for (path, structural) in changed_events {
            if installed
                .interests
                .iter()
                .any(|interest| interest.matches_event(&path, structural))
            {
                if structural {
                    needs_rescan = true;
                } else {
                    changed.push(path);
                }
            }
        }
        changed.sort();
        changed.dedup();
        pending_invalidation = if needs_rescan {
            Some(wake_app::WatchInvalidation::Rescan)
        } else if changed.is_empty() {
            None
        } else {
            Some(wake_app::WatchInvalidation::Paths(changed))
        };
    }
}

fn record_successful_build_diagnostics(
    state: &mut DashboardState,
    diagnostics: &[wake_app::DiagnosticInfo],
) {
    for diagnostic in diagnostics {
        state.diagnostic(diagnostic.clone(), format_diagnostic_plain(diagnostic));
    }
}

fn finish_watch(
    context: Option<&wake_app::BuildContext>,
    dashboard: &mut Option<Dashboard>,
    state: &mut DashboardState,
    ui: &Ui,
    reason: &str,
    exit_code: Option<u8>,
) -> Result<(), ExitCode> {
    state.stopping(reason);
    if let Some(active) = dashboard.as_mut() {
        let _ = active.draw(state);
    }
    if let Some(context) = context {
        context.close();
    }
    state.stopped();
    if let Some(mut active) = dashboard.take() {
        let _ = active.draw(state);
        active.restore();
    }
    ui.final_summary(
        "Watch stopped",
        "Watch",
        "typed project inputs",
        state.rebuilds,
        state.runtime(),
        reason,
    );
    match exit_code {
        Some(code) => Err(ExitCode::from(code)),
        None => Ok(()),
    }
}

fn is_build_watch_event(event: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn is_build_watch_structural_event(event: &notify::Event) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
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
    let mut state =
        DashboardState::new("dev", root, "LOCAL", "Live reload · source maps · watching");
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
        federation: None,
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
        "Demo · Controls · Live reload · watching"
    } else {
        "MDX · Live reload · search index · watching"
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
            federation: None,
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
            wake_app::DevServerEvent::RebuildStart {
                changed_paths,
                workspace,
                base_path,
            } => {
                let changed = changed_paths.len();
                state.rebuilding(changed_paths, workspace, base_path);
                if let Some(ui) = plain_ui {
                    ui.rebuild_start(changed);
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
                workspace,
                base_path,
            } => {
                let metrics = BuildMetrics {
                    modules,
                    updated_modules,
                    cached_modules,
                    chunks,
                    assets,
                    duration_ms,
                };
                state.built(metrics, initial, workspace, base_path);
                if let Some(ui) = plain_ui {
                    ui.rebuilt(metrics, initial);
                }
            }
            wake_app::DevServerEvent::Diagnostic { diagnostic } => {
                state.diagnostic(diagnostic.clone(), format_diagnostic_plain(&diagnostic));
                if let Some(ui) = plain_ui {
                    ui.diagnostic(&diagnostic);
                }
            }
            wake_app::DevServerEvent::WorkspaceState {
                total,
                loaded,
                failed,
                current,
                failed_names,
            } => state.workspace_state(total, loaded, failed, current, failed_names),
            wake_app::DevServerEvent::FederationUpdated { .. } => {
                // The browser-side dev coordinator owns remount-versus-reload behavior. The
                // dashboard deliberately keeps its current build metrics until the following
                // structured rebuild event arrives.
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
            presentation: None,
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
    } else if result.workspaces.is_empty() {
        format!("  {} {} routes", ui.dim("·"), result.routes.len())
    } else {
        format!(
            "  {} {} routes  {} {} workspaces",
            ui.dim("·"),
            result.routes.len(),
            ui.dim("·"),
            result.workspaces.len()
        )
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
        let source = SourceFile::new(file.display().to_string(), source_text.clone());
        let diagnostics = output
            .diagnostics
            .iter()
            .map(|diagnostic| wake_app::DiagnosticInfo::from_diagnostic(diagnostic, Some(&source)))
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
        let model = output.module.with_ast(wake_ecma_semantic::analyze);
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
        let source = SourceFile::new(file.display().to_string(), source_text.clone());
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
            .map(|diagnostic| wake_app::DiagnosticInfo::from_diagnostic(diagnostic, Some(&source)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn watch_plan(
        revision: u64,
        root: &Path,
        interests: Vec<wake_app::WatchInterest>,
    ) -> wake_app::WatchPlanSnapshot {
        wake_app::WatchPlanSnapshot {
            revision: wake_app::WatchPlanRevision(revision),
            root: root.to_path_buf(),
            interests,
        }
    }

    #[test]
    fn build_watch_capability_requires_complete_coverage_and_is_cleared_on_backend_loss() {
        let root = Path::new("project");
        let backend_generation = 11;
        let required = watch_plan(
            3,
            root,
            vec![wake_app::WatchInterest::tree(root.join("src"))],
        );
        let mut capability = BuildWatchCoverageCapability::default();
        let failed_generation = AtomicU64::new(0);
        assert_eq!(
            capability.revision_for(&required, backend_generation, &failed_generation),
            None
        );

        capability.confirm(watch_plan(3, root, Vec::new()), backend_generation);
        assert_eq!(
            capability.revision_for(&required, backend_generation, &failed_generation),
            None
        );

        capability.confirm(required.clone(), backend_generation);
        assert_eq!(
            capability.revision_for(&required, backend_generation, &failed_generation),
            Some(wake_app::WatchPlanRevision(3))
        );
        assert_eq!(
            capability.revision_for(&required, backend_generation + 1, &failed_generation,),
            None,
            "coverage from a failed backend generation is not transferable"
        );
        let wrong_revision = watch_plan(4, root, required.interests.clone());
        assert_eq!(
            capability.revision_for(&wrong_revision, backend_generation, &failed_generation),
            None,
            "an installed snapshot cannot attest a superseding revision"
        );
        let wrong_root = watch_plan(3, Path::new("other-project"), required.interests.clone());
        assert_eq!(
            capability.revision_for(&wrong_root, backend_generation, &failed_generation),
            None,
            "coverage is scoped to the plan root"
        );
        let failed_backend_cancellation = wake_app::CancellationToken::default();
        let successor_cancellation = wake_app::CancellationToken::default();
        revoke_build_watch_backend(
            &failed_generation,
            backend_generation,
            &failed_backend_cancellation,
        );
        assert!(failed_backend_cancellation.is_cancelled());
        assert!(
            !successor_cancellation.is_cancelled(),
            "a stale generation cannot cancel its successor"
        );
        let watermark = Arc::new(AtomicU64::new(0));
        let worker_watermark = Arc::clone(&watermark);
        let (cancel_entered_tx, cancel_entered_rx) = std::sync::mpsc::channel();
        let (cancel_release_tx, cancel_release_rx) = std::sync::mpsc::channel();
        let revoke = std::thread::spawn(move || {
            revoke_build_watch_backend_with(&worker_watermark, backend_generation, || {
                cancel_entered_tx.send(()).unwrap();
                cancel_release_rx.recv().unwrap();
            });
        });
        cancel_entered_rx.recv().unwrap();
        assert_eq!(
            watermark.load(Ordering::Acquire),
            backend_generation,
            "the backend watermark must be visible before cancellation waits on a commit"
        );
        cancel_release_tx.send(()).unwrap();
        revoke.join().unwrap();
        assert_eq!(
            capability.revision_for(&required, backend_generation, &failed_generation),
            None,
            "the callback's atomic backend revocation wins over queued rebuild work"
        );
        capability.clear();
        assert_eq!(
            capability.revision_for(&required, backend_generation, &failed_generation),
            None
        );
        assert!(is_current_build_watch_backend_error(
            Some(backend_generation),
            backend_generation
        ));
        assert!(
            !is_current_build_watch_backend_error(Some(backend_generation + 1), backend_generation),
            "a delayed error from a retired backend must not poison recovered state"
        );
    }

    #[test]
    fn build_watch_handoff_retains_bootstrap_coverage_until_context_ownership() {
        let root = Path::new("project");
        let bootstrap_interest = wake_app::WatchInterest::exact_file(root.join("wake.config.toml"));
        let context_interest = wake_app::WatchInterest::tree(root.join("src"));
        let bootstrap = watch_plan(7, root, vec![bootstrap_interest.clone()]);
        let context = watch_plan(2, root, vec![context_interest.clone()]);
        let handoff = build_watch_handoff_plan(&bootstrap, &context);
        assert_eq!(handoff.revision, context.revision);
        assert!(handoff.interests.contains(&bootstrap_interest));
        assert!(handoff.interests.contains(&context_interest));

        let mut capability = BuildWatchCoverageCapability::default();
        let backend_generation = 5;
        let failed_generation = AtomicU64::new(0);
        capability.confirm(handoff, backend_generation);
        assert_eq!(
            capability.revision_for(&context, backend_generation, &failed_generation),
            Some(context.revision)
        );
        assert!(capability.narrow_to(context.clone(), backend_generation, &failed_generation));
        assert!(
            !capability.confirm(context.clone(), backend_generation),
            "a bootstrap-only shrink must not schedule another build"
        );
        assert!(
            !build_watch_reconcile_needs_rescan(false, true, true),
            "registration cleanup after handoff must not schedule a duplicate build"
        );
        assert!(build_watch_reconcile_needs_rescan(false, true, false));

        let mut forward = BuildWatchCoverageCapability::default();
        let installed = watch_plan(
            2,
            root,
            vec![bootstrap_interest.clone(), context_interest.clone()],
        );
        forward.confirm(installed, backend_generation);
        let committed = watch_plan(3, root, vec![context_interest]);
        assert!(
            forward.narrow_to(committed.clone(), backend_generation, &failed_generation),
            "a successful candidate may advance revision while shrinking accepted coverage"
        );
        assert!(!forward.narrow_to(
            watch_plan(2, root, committed.interests.clone()),
            backend_generation,
            &failed_generation
        ));
        assert!(!forward.narrow_to(
            watch_plan(4, Path::new("other-project"), committed.interests),
            backend_generation,
            &failed_generation
        ));
        capability.clear();
        assert_eq!(
            capability.revision_for(&context, backend_generation, &failed_generation),
            None
        );
    }

    #[test]
    fn federation_init_command_parses_default_and_explicit_roots() {
        let default = Cli::try_parse_from(["wake", "federation", "init"]).unwrap();
        let Command::Federation {
            command: FederationCommand::Init { root },
        } = default.command
        else {
            panic!("expected federation init command");
        };
        assert_eq!(root, PathBuf::from("."));

        let explicit =
            Cli::try_parse_from(["wake", "federation", "init", "packages/catalog"]).unwrap();
        let Command::Federation {
            command: FederationCommand::Init { root },
        } = explicit.command
        else {
            panic!("expected federation init command");
        };
        assert_eq!(root, PathBuf::from("packages/catalog"));
    }

    #[test]
    fn federation_lock_command_parses_default_and_explicit_roots() {
        let default = Cli::try_parse_from(["wake", "federation", "lock"]).unwrap();
        let Command::Federation {
            command: FederationCommand::Lock { root },
        } = default.command
        else {
            panic!("expected federation lock command");
        };
        assert_eq!(root, PathBuf::from("."));

        let explicit =
            Cli::try_parse_from(["wake", "federation", "lock", "packages/shell"]).unwrap();
        let Command::Federation {
            command: FederationCommand::Lock { root },
        } = explicit.command
        else {
            panic!("expected federation lock command");
        };
        assert_eq!(root, PathBuf::from("packages/shell"));
    }

    #[test]
    fn test_error_category_only_applies_to_the_actual_test_subcommand() {
        let args = |values: &[&str]| values.iter().map(OsString::from).collect::<Vec<_>>();

        assert!(selects_test_command(&args(&["test", "--removed"])));
        assert!(selects_test_command(&args(&[
            "--no-color",
            "--ui=plain",
            "test",
            "--removed",
        ])));
        assert!(!selects_test_command(&args(&[
            "build",
            "test",
            "--removed"
        ])));
        assert!(!selects_test_command(&args(&["--removed", "test"])));
    }

    #[test]
    fn test_watch_keys_map_to_pure_actions() {
        use crossterm::event::{KeyCode, KeyModifiers};

        for (code, modifiers, expected) in [
            (
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                TestWatchKeyAction::All,
            ),
            (
                KeyCode::Char('f'),
                KeyModifiers::NONE,
                TestWatchKeyAction::Failed,
            ),
            (
                KeyCode::Char('p'),
                KeyModifiers::NONE,
                TestWatchKeyAction::PromptPath,
            ),
            (
                KeyCode::Char('t'),
                KeyModifiers::NONE,
                TestWatchKeyAction::PromptName,
            ),
            (
                KeyCode::Char('u'),
                KeyModifiers::NONE,
                TestWatchKeyAction::UpdateSnapshots,
            ),
            (
                KeyCode::Char('r'),
                KeyModifiers::NONE,
                TestWatchKeyAction::Rerun,
            ),
            (
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                TestWatchKeyAction::Quit,
            ),
            (
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                TestWatchKeyAction::Interrupt,
            ),
            (
                KeyCode::Char('c'),
                KeyModifiers::NONE,
                TestWatchKeyAction::Ignore,
            ),
            (
                KeyCode::Enter,
                KeyModifiers::NONE,
                TestWatchKeyAction::Ignore,
            ),
        ] {
            assert_eq!(test_watch_key_action(code, modifiers), expected);
        }
    }

    #[test]
    fn test_command_accepts_only_the_wake_dashed_contract() {
        let cli = Cli::try_parse_from([
            "wake",
            "test",
            "src/**/*.test.tsx",
            "--root",
            "fixture",
            "--name-pattern",
            "renders",
            "--project",
            "client",
            "--project",
            "browser",
            "--environment",
            "dom",
            "--watch",
            "--related",
            "src/button.tsx",
            "src/dialog.tsx",
            "--coverage",
            "--update-snapshots",
            "--workers",
            "50%",
            "--bail",
            "--shard",
            "2/3",
            "--seed",
            "release-21",
            "--shuffle",
            "--reporter",
            "json",
            "--output",
            "reports/tests.json",
            "--allow-no-tests",
            "--browser-path",
            "chromium",
            "--headful",
        ])
        .unwrap();

        let Command::Test {
            patterns,
            root,
            name_pattern,
            projects,
            environment,
            watch,
            changed,
            related,
            coverage,
            update_snapshots,
            serial,
            workers,
            bail,
            shard,
            seed,
            shuffle,
            reporter,
            output,
            allow_no_tests,
            browser_path,
            headful,
        } = cli.command
        else {
            panic!("expected test command");
        };
        assert_eq!(patterns, ["src/**/*.test.tsx"]);
        assert_eq!(root, PathBuf::from("fixture"));
        assert_eq!(name_pattern.as_deref(), Some("renders"));
        assert_eq!(projects, ["client", "browser"]);
        assert_eq!(environment, Some(TestEnvironmentArg::Dom));
        assert!(watch);
        assert!(!changed);
        assert_eq!(
            related,
            [
                PathBuf::from("src/button.tsx"),
                PathBuf::from("src/dialog.tsx")
            ]
        );
        assert!(coverage);
        assert!(update_snapshots);
        assert!(!serial);
        assert_eq!(workers.as_deref(), Some("50%"));
        assert_eq!(bail, Some(1));
        assert_eq!(shard.as_deref(), Some("2/3"));
        assert_eq!(seed.as_deref(), Some("release-21"));
        assert!(shuffle);
        assert_eq!(reporter, Some(TestReporterArg::Json));
        assert_eq!(output, Some(PathBuf::from("reports/tests.json")));
        assert!(allow_no_tests);
        assert_eq!(browser_path, Some(PathBuf::from("chromium")));
        assert!(headful);
    }

    #[test]
    fn test_command_rejects_removed_jest_flags() {
        for flag in [
            "--testNamePattern",
            "--test-name-pattern",
            "--runInBand",
            "--run-in-band",
            "--updateSnapshot",
            "--update-snapshot",
            "--passWithNoTests",
            "--pass-with-no-tests",
            "--watchAll",
            "--watch-all",
            "--config",
            "--init",
            "--json",
            "--randomize",
        ] {
            assert!(
                Cli::try_parse_from(["wake", "test", flag]).is_err(),
                "unexpectedly accepted {flag}"
            );
        }
    }

    #[test]
    fn test_worker_and_shard_values_are_validated_during_cli_parsing() {
        for workers in ["0", "0%", "101%", "half"] {
            assert!(
                Cli::try_parse_from(["wake", "test", "--workers", workers]).is_err(),
                "unexpectedly accepted workers={workers}"
            );
        }
        for shard in ["0/1", "2/1", "1/0", "1", "1/2/3"] {
            assert!(
                Cli::try_parse_from(["wake", "test", "--shard", shard]).is_err(),
                "unexpectedly accepted shard={shard}"
            );
        }
    }

    #[test]
    fn serial_and_changed_modes_reject_conflicting_overrides() {
        assert!(Cli::try_parse_from(["wake", "test", "--serial", "--workers", "2"]).is_err());
        assert!(
            Cli::try_parse_from(["wake", "test", "--changed", "--related", "src/button.tsx"])
                .is_err()
        );
    }

    #[test]
    fn worker_overrides_preserve_numeric_and_text_protocol_shapes() {
        assert_eq!(
            test_worker_override("3".to_string()),
            wake_app::WorkerOverride::Count(3)
        );
        assert_eq!(
            test_worker_override("auto".to_string()),
            wake_app::WorkerOverride::Text("auto".to_string())
        );
        assert_eq!(
            test_worker_override("50%".to_string()),
            wake_app::WorkerOverride::Text("50%".to_string())
        );
    }

    #[test]
    fn test_exit_codes_follow_the_approved_contract() {
        assert_eq!(test_result_exit(&test_result(true, "completed")), Ok(()));
        assert_eq!(
            test_result_exit(&test_result(false, "completed")),
            Err(ExitCode::FAILURE)
        );
        assert_eq!(
            test_result_exit(&test_result(false, "cancelled")),
            Err(ExitCode::from(130))
        );
        assert_eq!(
            test_result_exit(&test_result(false, "host-crash")),
            Err(ExitCode::from(2))
        );
    }

    #[test]
    fn junit_report_uses_structured_counts_and_failures() {
        let result = serde_json::from_value::<wake_app::TestRunResult>(serde_json::json!({
            "schemaVersion": "wake.test.v1",
            "runId": "run-1",
            "success": false,
            "seed": "seed-1",
            "durationMs": 1250,
            "terminationReason": "completed",
            "environment": test_environment(),
            "suites": [{
                "id": "suite-1",
                "path": "src/button.test.tsx",
                "name": "button.test",
                "project": "client",
                "environment": test_environment(),
                "status": "failed",
                "durationMs": 1000,
                "tests": [{
                    "id": "test-1",
                    "name": "renders",
                    "fullName": "Button renders",
                    "status": "failed",
                    "durationMs": 20,
                    "assertions": 1,
                    "attempts": 1,
                    "location": null,
                    "failures": [{
                        "message": "expected <button>",
                        "code": "WAKE_TEST_ASSERTION",
                        "stack": null,
                        "location": null,
                        "diff": null
                    }]
                }],
                "failures": [{
                    "message": "setup & cleanup failed",
                    "code": "WAKE_TEST_RUNTIME",
                    "stack": null,
                    "location": null,
                    "diff": null
                }],
                "snapshot": null
            }],
            "counts": {
                "suites": {"total": 1, "passed": 0, "failed": 1, "skipped": 0},
                "tests": {"total": 1, "passed": 0, "failed": 1, "skipped": 0, "todo": 0}
            },
            "snapshot": {
                "added": 0,
                "matched": 0,
                "unmatched": 0,
                "updated": 0,
                "obsolete": 0,
                "filesRemoved": 0
            },
            "coverage": null,
            "leaks": [],
            "artifacts": [],
            "diagnostics": []
        }))
        .unwrap();

        let report = junit_test_report(&result);
        assert!(report.contains(
            "<testsuites tests=\"2\" failures=\"1\" errors=\"1\" skipped=\"0\" time=\"1.250\">"
        ));
        assert!(report.contains("WAKE_TEST_ASSERTION: expected &lt;button&gt;"));
        assert!(report.contains("WAKE_TEST_RUNTIME: setup &amp; cleanup failed"));
    }

    fn test_result(success: bool, termination_reason: &str) -> wake_app::TestRunResult {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": "wake.test.v1",
            "runId": "run-1",
            "success": success,
            "seed": "seed-1",
            "durationMs": 0,
            "terminationReason": termination_reason,
            "environment": test_environment(),
            "suites": [],
            "counts": {
                "suites": {"total": 0, "passed": 0, "failed": 0, "skipped": 0},
                "tests": {"total": 0, "passed": 0, "failed": 0, "skipped": 0, "todo": 0}
            },
            "snapshot": {
                "added": 0,
                "matched": 0,
                "unmatched": 0,
                "updated": 0,
                "obsolete": 0,
                "filesRemoved": 0
            },
            "coverage": null,
            "leaks": [],
            "artifacts": [],
            "diagnostics": []
        }))
        .unwrap()
    }

    fn test_environment() -> serde_json::Value {
        serde_json::json!({
            "kind": "dom",
            "react": "19.2.0",
            "reactDom": "19.2.0",
            "v8": "15.0",
            "browser": null
        })
    }
}
