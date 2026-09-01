//! Reusable Wake application services.
//!
//! This crate owns project/configuration orchestration. Frontends such as the
//! Rust CLI and the Node-API addon are responsible only for argument parsing,
//! presentation, and process lifecycle.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use wake_bundler::{BuildOutput, BuildRequest, BuildSession, IncrementalBundler, ResolveOptions};
pub use wake_bundler::{BuildPlatform, ModuleFormat};
use wake_common::{Diagnostic, OsFileSystem, SourceFile};

pub use wake_docs::{DocsMode, DocsPresentation};
use wake_ecma_transform::{BrowserTarget, TargetEnv};
pub use wake_test_contract::protocol::WatchControl as TestWatchControl;
use wake_test_contract::protocol::{
    FrameDecoder, HOST_BUILD_ID, HostAck, HostCommand, HostError, HostEvent, HostHello,
    HostRequest, HostResponse, HostResponseBody, PROTOCOL_VERSION, WatchControl, write_frame,
};
pub use wake_test_contract::{
    TestCaseResult, TestDiagnostic, TestFailure, TestOptions, TestRunResult, TestStatus,
    TestSuiteResult, TestTerminationReason, WorkerOverride,
};

mod library;
pub use library::{
    GenerateCssTokenOptions, GenerateCssTokenResult, GenerateDocgenOptions, GenerateDocgenResult,
    LibraryBuildOptions, LibraryBuildResult, build_library, generate_css_token, generate_docgen,
};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<DiagnosticInfo>,
}

impl WakeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            path: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn at(mut self, path: &Path) -> Self {
        self.path = Some(path.to_string_lossy().into_owned());
        self
    }

    pub fn with_diagnostics(mut self, diagnostics: &[Diagnostic]) -> Self {
        self.diagnostics = diagnostics
            .iter()
            .map(|diagnostic| DiagnosticInfo::from_diagnostic(diagnostic, None))
            .collect();
        self
    }

    fn with_diagnostic_infos(mut self, diagnostics: Vec<DiagnosticInfo>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub fn cancelled() -> Self {
        Self::new("WAKE_CANCELLED", "Wake operation was cancelled")
    }

    pub fn closed(resource: &str) -> Self {
        Self::new(
            "WAKE_INTERNAL",
            format!("{resource} has already been closed"),
        )
    }
}

impl std::fmt::Display for WakeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WakeError {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticInfo {
    pub severity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<DiagnosticLocation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticLocation {
    /// One-based source line.
    pub line: u32,
    /// One-based Unicode-scalar column.
    pub column: u32,
    /// One-based source line containing the exclusive end offset.
    pub end_line: u32,
    /// One-based Unicode-scalar column of the exclusive end offset.
    pub end_column: u32,
    /// Exact source line without its line terminator.
    pub line_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl DiagnosticInfo {
    pub fn from_diagnostic(value: &Diagnostic, source: Option<&SourceFile>) -> Self {
        let span = value.primary_span();
        let primary_label = value
            .labels
            .iter()
            .find(|label| label.primary)
            .or_else(|| value.labels.first());
        let location = span
            .filter(|span| {
                source.is_some_and(|source| {
                    span.lo <= span.hi
                        && span.hi <= source.len()
                        && source.src().is_char_boundary(span.lo as usize)
                        && source.src().is_char_boundary(span.hi as usize)
                })
            })
            .and_then(|span| {
                let source = source?;
                let start = source.location(span.lo);
                let end = source.location(span.hi);
                Some(DiagnosticLocation {
                    line: start.line,
                    column: start.column,
                    end_line: end.line,
                    end_column: end.column,
                    line_text: source
                        .line_text(start.line.saturating_sub(1) as usize)
                        .to_string(),
                    label: primary_label.and_then(|label| label.message.clone()),
                })
            });
        Self {
            severity: value.severity.as_str().to_string(),
            code: value.code.as_ref().map(ToString::to_string),
            message: value.message.clone(),
            path: value.path.clone(),
            start: span.map(|span| span.lo),
            end: span.map(|span| span.hi),
            location,
            notes: value.notes.clone(),
        }
    }
}

fn diagnostic_infos(diagnostics: &[Diagnostic], root: &Path) -> Vec<DiagnosticInfo> {
    let mut sources = HashMap::<String, Option<SourceFile>>::new();
    diagnostics
        .iter()
        .map(|diagnostic| {
            let source = diagnostic.path.as_deref().and_then(|path| {
                sources
                    .entry(path.to_string())
                    .or_insert_with(|| {
                        let path_buf = PathBuf::from(path);
                        let resolved = if path_buf.is_absolute() {
                            path_buf
                        } else {
                            root.join(path_buf)
                        };
                        std::fs::read_to_string(resolved)
                            .ok()
                            .map(|text| SourceFile::new(path, text))
                    })
                    .as_ref()
            });
            DiagnosticInfo::from_diagnostic(diagnostic, source)
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
pub struct ProjectOptions {
    pub cwd: Option<PathBuf>,
    pub config_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub outdir: Option<PathBuf>,
    pub cache: bool,
    pub source_map: bool,
    pub write: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            project: ProjectOptions::default(),
            entry: None,
            outdir: None,
            cache: false,
            source_map: false,
            write: true,
        }
    }
}

/// 单文件 library bundle 选项。与 Web application build 明确分离。
#[derive(Debug, Clone, Default)]
pub struct BundleOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub outfile: Option<PathBuf>,
    pub platform: Option<BuildPlatform>,
    pub format: Option<ModuleFormat>,
    pub target: Option<String>,
    pub external: Vec<String>,
    pub minify: bool,
    pub source_map: bool,
    pub cache: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFile {
    pub path: String,
    pub kind: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildResult {
    pub success: bool,
    pub module_count: usize,
    pub updated_module_count: usize,
    pub cached_module_count: usize,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    pub files: Vec<OutputFile>,
    pub diagnostics: Vec<DiagnosticInfo>,
}

/// 单文件 bundle 的结果。与 Web build 的目录结果分离，并用类型保证始终返回代码。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleResult {
    pub success: bool,
    pub module_count: usize,
    pub updated_module_count: usize,
    pub cached_module_count: usize,
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_file: Option<String>,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_map_file: Option<String>,
    pub files: Vec<OutputFile>,
    pub diagnostics: Vec<DiagnosticInfo>,
}

#[derive(Debug, Clone)]
struct ResolvedBundleOptions {
    project: ProjectOptions,
    entry: Option<PathBuf>,
    outfile: Option<PathBuf>,
    platform: BuildPlatform,
    format: ModuleFormat,
    target: Option<String>,
    external: Vec<String>,
    minify: bool,
    source_map: bool,
    cache: bool,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), WakeError> {
        if self.is_cancelled() {
            Err(WakeError::cancelled())
        } else {
            Ok(())
        }
    }
}

struct PreparedBuild {
    root: PathBuf,
    entry: PathBuf,
    outdir: PathBuf,
    config: wake_config::Config,
    aliases: Vec<(String, PathBuf)>,
}

type PreparedDocs = (
    PreparedBuild,
    wake_docs::DocsOptions,
    Vec<wake_docs::RouteInfo>,
    Vec<wake_docs::DemoDescriptor>,
    Vec<String>,
);

pub fn build(
    options: BuildOptions,
    cancellation: &CancellationToken,
) -> Result<BuildResult, WakeError> {
    execute_build(options, cancellation, true)
}

pub fn bundle(
    options: BundleOptions,
    cancellation: &CancellationToken,
) -> Result<BundleResult, WakeError> {
    execute_bundle(options, cancellation)
}

/// Run native JavaScript tests through the crash-isolated Wake test host.
pub fn run_tests(
    options: TestOptions,
    cancellation: &CancellationToken,
) -> Result<TestRunResult, WakeError> {
    run_tests_with_host(options, None, cancellation)
}

/// Run tests with an explicit packaged host path.
///
/// Node's package loader supplies this path because the Node executable itself is not installed
/// beside the platform package. Other frontends normally use [`run_tests`].
pub fn run_tests_with_host(
    options: TestOptions,
    host_path: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<TestRunResult, WakeError> {
    let mut session = TestSession::start_with_host(host_path, cancellation)?;
    let outcome = session.run(options, cancellation);
    let close = session.close();
    match (outcome, close) {
        (Ok(result), Ok(())) => Ok(result),
        (Ok(_), Err(error)) | (Err(error), Ok(())) => Err(error),
        (Err(mut error), Err(close_error)) => {
            error.message.push_str("; test-host shutdown failed: ");
            error.message.push_str(&close_error.message);
            Err(error)
        }
    }
}

fn reap_test_host(child: &mut Child) {
    for _ in 0..100 {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

const TEST_HOST_POLL_INTERVAL: Duration = Duration::from_millis(25);
const TEST_HOST_READER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const TEST_HOST_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
static NEXT_TEST_RUN_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_TEST_WATCH_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TestSessionEvent {
    RunStart {
        run_id: String,
        watching: bool,
    },
    TestCaseResult {
        run_id: String,
        suite_id: String,
        result: Box<TestCaseResult>,
    },
    SuiteResult {
        run_id: String,
        result: Box<TestSuiteResult>,
    },
    Diagnostic {
        run_id: Option<String>,
        diagnostic: Box<TestDiagnostic>,
    },
    RunComplete {
        result: Box<TestRunResult>,
    },
    Closed,
}

/// A persistent client session for the isolated Wake test host.
pub struct TestSession {
    child: Child,
    stderr: Option<ChildStderr>,
    protocol: Option<TestProtocolSession>,
    events: Vec<TestSessionEvent>,
    closed: bool,
}

impl TestSession {
    pub fn start(cancellation: &CancellationToken) -> Result<Self, WakeError> {
        Self::start_with_host(None, cancellation)
    }

    pub fn start_with_host(
        explicit_host: Option<&Path>,
        cancellation: &CancellationToken,
    ) -> Result<Self, WakeError> {
        cancellation.check()?;
        let host_path = resolve_test_host(explicit_host)?;
        let token = test_host_token()?;
        let mut child = Command::new(&host_path)
            .arg("--token")
            .arg(&token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                WakeError::new(
                    "WAKE_TEST_HOST",
                    format!("could not start test host: {error}"),
                )
                .at(&host_path)
            })?;
        match connect_test_host(&mut child, &token, cancellation) {
            Ok(protocol) => {
                let stderr = child.stderr.take();
                Ok(Self {
                    child,
                    stderr,
                    protocol: Some(protocol),
                    events: Vec::new(),
                    closed: false,
                })
            }
            Err(mut error) => {
                let mut stderr = child.stderr.take();
                let _ = child.kill();
                reap_test_host(&mut child);
                append_test_host_stderr(&mut error, &mut stderr);
                Err(error)
            }
        }
    }

    pub fn run(
        &mut self,
        options: TestOptions,
        cancellation: &CancellationToken,
    ) -> Result<TestRunResult, WakeError> {
        if self.closed {
            return Err(WakeError::closed("TestSession"));
        }
        let protocol = self.protocol.as_mut().ok_or_else(|| {
            WakeError::new("WAKE_TEST_HOST", "test-host protocol is not connected")
        })?;
        let result = protocol.run(options, cancellation);
        self.events.extend(protocol.drain_events());
        result
    }

    pub fn start_watch(&mut self, options: TestOptions) -> Result<(), WakeError> {
        if self.closed {
            return Err(WakeError::closed("TestSession"));
        }
        self.protocol
            .as_mut()
            .ok_or_else(|| WakeError::new("WAKE_TEST_HOST", "test-host protocol is not connected"))?
            .start_watch(options)
    }

    pub fn stop_watch(&mut self) -> Result<(), WakeError> {
        if self.closed {
            return Ok(());
        }
        self.protocol
            .as_mut()
            .ok_or_else(|| WakeError::new("WAKE_TEST_HOST", "test-host protocol is not connected"))?
            .stop_watch()
    }

    pub fn is_watching(&self) -> bool {
        self.protocol
            .as_ref()
            .is_some_and(TestProtocolSession::is_watching)
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn watch_control(&mut self, control: TestWatchControl) -> Result<(), WakeError> {
        if self.closed {
            return Err(WakeError::closed("TestSession"));
        }
        self.protocol
            .as_mut()
            .ok_or_else(|| WakeError::new("WAKE_TEST_HOST", "test-host protocol is not connected"))?
            .watch_control(control)
    }

    pub fn poll_events(&mut self) -> Result<(), WakeError> {
        if self.closed {
            return Ok(());
        }
        let protocol = self.protocol.as_mut().ok_or_else(|| {
            WakeError::new("WAKE_TEST_HOST", "test-host protocol is not connected")
        })?;
        let outcome = protocol.poll_watch_events();
        self.events.extend(protocol.drain_events());
        outcome
    }

    pub fn drain_events(&mut self) -> Vec<TestSessionEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn close(&mut self) -> Result<(), WakeError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let mut outcome = Ok(());
        if let Some(mut protocol) = self.protocol.take() {
            if !protocol.closed {
                if protocol.is_watching()
                    && let Err(error) = protocol.stop_watch()
                {
                    outcome = Err(error);
                }
                if outcome.is_ok()
                    && let Err(error) = protocol.wait_for_watch_idle()
                {
                    outcome = Err(error);
                }
                if outcome.is_ok()
                    && let Err(error) = protocol.shutdown()
                {
                    outcome = Err(error);
                }
            }
            protocol.close_transport();
            self.events.extend(protocol.drain_events());
        }
        if outcome.is_err() {
            let _ = self.child.kill();
        }
        reap_test_host(&mut self.child);
        if let Err(error) = &mut outcome {
            append_test_host_stderr(error, &mut self.stderr);
        }
        if !self
            .events
            .iter()
            .any(|event| matches!(event, TestSessionEvent::Closed))
        {
            self.events.push(TestSessionEvent::Closed);
        }
        outcome
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn connect_test_host(
    child: &mut Child,
    token: &str,
    cancellation: &CancellationToken,
) -> Result<TestProtocolSession, WakeError> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| WakeError::new("WAKE_TEST_HOST", "test host stdout was not connected"))?;
    let (handshake_sender, handshake_receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut stdout = BufReader::new(stdout);
        let mut handshake = String::new();
        let result = stdout.read_line(&mut handshake).map(|_| handshake);
        let _ = handshake_sender.send(result);
    });
    let handshake_started = Instant::now();
    let handshake = loop {
        match handshake_receiver.recv_timeout(TEST_HOST_POLL_INTERVAL) {
            Ok(Ok(handshake)) => break handshake,
            Ok(Err(error)) => {
                return Err(WakeError::new(
                    "WAKE_TEST_HOST",
                    format!("could not read test host handshake: {error}"),
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                cancellation.check()?;
                if handshake_started.elapsed() >= TEST_HOST_CONTROL_TIMEOUT {
                    return Err(WakeError::new(
                        "WAKE_TEST_HOST",
                        "test host did not complete its handshake within 10 seconds",
                    ));
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(WakeError::new(
                    "WAKE_TEST_HOST",
                    "test host closed stdout before its handshake",
                ));
            }
        }
    };
    let hello = serde_json::from_str::<HostHello>(handshake.trim()).map_err(|error| {
        WakeError::new(
            "WAKE_TEST_HOST",
            format!("test host returned an invalid handshake: {error}"),
        )
    })?;
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(WakeError::new(
            "WAKE_TEST_HOST",
            format!(
                "test host protocol mismatch: application {}, host {}",
                PROTOCOL_VERSION, hello.protocol_version
            ),
        ));
    }
    if hello.build_id != HOST_BUILD_ID {
        return Err(WakeError::new(
            "WAKE_TEST_HOST",
            format!(
                "test host build mismatch: application {}, host {}",
                HOST_BUILD_ID, hello.build_id
            ),
        ));
    }
    let address = hello
        .address
        .parse::<std::net::SocketAddr>()
        .map_err(|error| {
            WakeError::new(
                "WAKE_TEST_HOST",
                format!("test host returned an invalid address: {error}"),
            )
        })?;
    if !address.ip().is_loopback() {
        return Err(WakeError::new(
            "WAKE_TEST_HOST",
            "test host attempted to use a non-loopback address",
        ));
    }
    cancellation.check()?;

    let stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(10)).map_err(|error| {
            WakeError::new(
                "WAKE_TEST_HOST",
                format!("could not connect to test host: {error}"),
            )
        })?;
    TestProtocolSession::new(stream, token.to_string())
}

fn append_test_host_stderr(error: &mut WakeError, stderr: &mut Option<ChildStderr>) {
    let Some(stderr) = stderr else {
        return;
    };
    let mut details = String::new();
    let _ = stderr.read_to_string(&mut details);
    if !details.trim().is_empty() {
        error.message.push_str("; host stderr: ");
        error.message.push_str(details.trim());
    }
}

enum TestProtocolIncoming {
    Response(Box<HostResponse>),
    Closed,
    DecodeError(String),
}

struct TestProtocolSession {
    writer: TcpStream,
    token: String,
    incoming: mpsc::Receiver<TestProtocolIncoming>,
    reader_stop: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<()>>,
    next_request_id: u64,
    next_sequence: u64,
    watch_id: Option<String>,
    watch_request_id: Option<u64>,
    watch_run_active: bool,
    watch_run_id: Option<String>,
    events: Vec<TestSessionEvent>,
    closed: bool,
}

impl TestProtocolSession {
    fn new(writer: TcpStream, token: String) -> Result<Self, WakeError> {
        writer
            .set_write_timeout(Some(TEST_HOST_CONTROL_TIMEOUT))
            .map_err(|error| test_protocol_error(format!("could not configure writer: {error}")))?;
        let reader_stream = writer
            .try_clone()
            .map_err(|error| test_protocol_error(format!("could not clone socket: {error}")))?;
        let (sender, incoming) = mpsc::channel();
        let reader_stop = Arc::new(AtomicBool::new(false));
        let stop = reader_stop.clone();
        let reader = thread::Builder::new()
            .name("wake-test-client-reader".to_string())
            .spawn(move || read_test_protocol_frames(reader_stream, &sender, &stop))
            .map_err(|error| {
                test_protocol_error(format!("could not create response reader: {error}"))
            })?;
        Ok(Self {
            writer,
            token,
            incoming,
            reader_stop,
            reader: Some(reader),
            next_request_id: 0,
            next_sequence: 0,
            watch_id: None,
            watch_request_id: None,
            watch_run_active: false,
            watch_run_id: None,
            events: Vec::new(),
            closed: false,
        })
    }

    fn run(
        &mut self,
        options: TestOptions,
        cancellation: &CancellationToken,
    ) -> Result<TestRunResult, WakeError> {
        cancellation.check()?;
        let run_id = format!(
            "wake-run-{}-{}",
            std::process::id(),
            NEXT_TEST_RUN_ID.fetch_add(1, Ordering::Relaxed)
        );
        let run_request = self.send_command(HostCommand::Run {
            run_id: run_id.clone(),
            options: Box::new(options),
        })?;
        let mut cancel_request = None;
        let mut cancel_done = false;
        let mut run_acknowledged = false;
        let mut terminal: Option<Result<TestRunResult, WakeError>> = None;

        loop {
            if cancellation.is_cancelled() && cancel_request.is_none() && terminal.is_none() {
                cancel_request = Some(self.send_command(HostCommand::Cancel {
                    run_id: run_id.clone(),
                })?);
            }
            if (cancel_request.is_none() || cancel_done)
                && let Some(terminal) = terminal.take()
            {
                if let Ok(result) = &terminal {
                    self.events.push(TestSessionEvent::RunComplete {
                        result: Box::new(result.clone()),
                    });
                }
                return terminal;
            }

            let Some(response) = self.receive_response()? else {
                continue;
            };
            if self.watch_request_id == Some(response.request_id) {
                self.record_watch_response(response)?;
                continue;
            }
            let is_run_response = response.request_id == run_request;
            let is_cancel_response = cancel_request == Some(response.request_id);
            if !is_run_response && !is_cancel_response {
                return Err(test_protocol_error(format!(
                    "response {} does not match run request {}{}",
                    response.request_id,
                    run_request,
                    cancel_request.map_or_else(String::new, |request| format!(
                        " or cancel request {request}"
                    ))
                )));
            }

            match response.body {
                HostResponseBody::Ack { command } => match command {
                    HostAck::Run {
                        run_id: acknowledged,
                    } if is_run_response && acknowledged == run_id => {
                        run_acknowledged = true;
                    }
                    HostAck::Cancel { run_id: cancelled }
                        if is_cancel_response && cancelled == run_id =>
                    {
                        cancel_done = true;
                    }
                    _ => {
                        return Err(test_protocol_error(
                            "test host returned an acknowledgement for the wrong command",
                        ));
                    }
                },
                HostResponseBody::Event { event } => {
                    if !is_run_response || !run_acknowledged {
                        return Err(test_protocol_error(
                            "test host emitted a run event before acknowledging the run",
                        ));
                    }
                    self.record_event(&run_id, *event)?;
                }
                HostResponseBody::Result {
                    run_id: completed,
                    result,
                } => {
                    if !is_run_response || !run_acknowledged {
                        return Err(test_protocol_error(
                            "test host returned a result before acknowledging the run",
                        ));
                    }
                    if completed != run_id || result.run_id != run_id {
                        return Err(test_protocol_error(format!(
                            "test host returned result `{completed}` for active run `{run_id}`"
                        )));
                    }
                    terminal = Some(Ok(*result));
                }
                HostResponseBody::Error { error, .. } if is_cancel_response => {
                    if error.code == "WAKE_TEST_UNKNOWN_RUN" {
                        cancel_done = true;
                    } else {
                        return Err(wake_error_from_host(error));
                    }
                }
                HostResponseBody::Error { error, .. } => {
                    terminal = Some(Err(wake_error_from_host(error)));
                    if run_acknowledged {
                        self.events.push(TestSessionEvent::Closed);
                        self.close_transport();
                    }
                }
                HostResponseBody::WatchRunError { .. } => {
                    return Err(test_protocol_error(
                        "test host emitted a watch terminal on a run request",
                    ));
                }
            }
        }
    }

    fn start_watch(&mut self, options: TestOptions) -> Result<(), WakeError> {
        if self.watch_id.is_some() {
            return Ok(());
        }
        let watch_id = format!(
            "wake-watch-{}-{}",
            std::process::id(),
            NEXT_TEST_WATCH_ID.fetch_add(1, Ordering::Relaxed)
        );
        let request = self.send_command(HostCommand::StartWatch {
            watch_id: watch_id.clone(),
            options: Box::new(options),
        })?;
        self.await_control_ack(request, |command| {
            matches!(command, HostAck::StartWatch { watch_id: active } if active == &watch_id)
        })?;
        self.watch_id = Some(watch_id);
        self.watch_request_id = Some(request);
        let started = Instant::now();
        loop {
            if started.elapsed() >= TEST_HOST_CONTROL_TIMEOUT {
                return Err(test_protocol_error(
                    "test host did not report a ready filesystem watcher within 10 seconds",
                ));
            }
            let Some(response) = self.receive_response()? else {
                continue;
            };
            if response.request_id != request {
                return Err(test_protocol_error(
                    "test host returned an unrelated response while starting watch",
                ));
            }
            match response.body {
                HostResponseBody::Event { event } if matches!(&*event, HostEvent::WatchReady { watch_id: ready, .. } if ready == self.watch_id.as_deref().unwrap_or_default()) =>
                {
                    break;
                }
                HostResponseBody::Error { error, .. } => return Err(wake_error_from_host(error)),
                HostResponseBody::WatchRunError { error, .. } => {
                    return Err(wake_error_from_host(error));
                }
                _ => {
                    return Err(test_protocol_error(
                        "test host returned the wrong watch-ready response",
                    ));
                }
            }
        }
        Ok(())
    }

    fn stop_watch(&mut self) -> Result<(), WakeError> {
        let Some(watch_id) = self.watch_id.clone() else {
            return Ok(());
        };
        let request = self.send_command(HostCommand::StopWatch {
            watch_id: watch_id.clone(),
        })?;
        self.await_control_ack(request, |command| {
            matches!(command, HostAck::StopWatch { watch_id: active } if active == &watch_id)
        })?;
        self.watch_id = None;
        // The public stop boundary is quiescent: callers may immediately run, restart watch, or
        // shut down without racing the cancelled worker that belonged to the old watch.
        self.wait_for_watch_idle()
    }

    fn watch_control(&mut self, control: WatchControl) -> Result<(), WakeError> {
        let watch_id = self
            .watch_id
            .clone()
            .ok_or_else(|| WakeError::new("WAKE_TEST_UNKNOWN_WATCH", "no test watch is active"))?;
        let request = self.send_command(HostCommand::WatchControl {
            watch_id: watch_id.clone(),
            control,
        })?;
        self.await_control_ack(request, |command| {
            matches!(command, HostAck::WatchControl { watch_id: active } if active == &watch_id)
        })
    }

    fn shutdown(&mut self) -> Result<(), WakeError> {
        if self.closed {
            return Ok(());
        }
        let request = self.send_command(HostCommand::Shutdown)?;
        self.await_control_ack(request, |command| matches!(command, HostAck::Shutdown))?;
        self.closed = true;
        Ok(())
    }

    fn is_watching(&self) -> bool {
        self.watch_id.is_some()
    }

    fn poll_watch_events(&mut self) -> Result<(), WakeError> {
        loop {
            let Some(response) = self.try_receive_response()? else {
                return Ok(());
            };
            if self.watch_request_id != Some(response.request_id) {
                return Err(test_protocol_error(format!(
                    "unsolicited response {} does not belong to the active watch",
                    response.request_id
                )));
            }
            self.record_watch_response(response)?;
        }
    }

    fn wait_for_watch_idle(&mut self) -> Result<(), WakeError> {
        let started = Instant::now();
        while self.watch_run_active {
            if started.elapsed() >= TEST_HOST_CONTROL_TIMEOUT {
                return Err(test_protocol_error(
                    "watch run did not stop within 10 seconds",
                ));
            }
            let Some(response) = self.receive_response()? else {
                continue;
            };
            if self.watch_request_id != Some(response.request_id) {
                return Err(test_protocol_error(
                    "received an unrelated response while stopping watch",
                ));
            }
            self.record_watch_response(response)?;
        }
        self.watch_request_id = None;
        Ok(())
    }

    fn record_watch_response(&mut self, response: HostResponse) -> Result<(), WakeError> {
        match response.body {
            HostResponseBody::Event { event } => match *event {
                HostEvent::RunStart { run_id, watching } if watching => {
                    if self.watch_run_active {
                        return Err(test_protocol_error(
                            "test host started a second watch run before completing the first",
                        ));
                    }
                    self.watch_run_active = true;
                    self.watch_run_id = Some(run_id.clone());
                    self.events
                        .push(TestSessionEvent::RunStart { run_id, watching });
                }
                HostEvent::RunComplete {
                    watch_id,
                    run_id,
                    result,
                } => {
                    if self
                        .watch_id
                        .as_deref()
                        .is_some_and(|active| active != watch_id)
                        || self.watch_run_id.as_deref() != Some(run_id.as_str())
                        || result.run_id != run_id
                    {
                        return Err(test_protocol_error(
                            "test host completed a run for an unrelated watch",
                        ));
                    }
                    self.watch_run_active = false;
                    self.watch_run_id = None;
                    self.events.push(TestSessionEvent::RunComplete { result });
                    if self.watch_id.is_none() {
                        self.watch_request_id = None;
                    }
                }
                HostEvent::WatchReady { .. } => {}
                HostEvent::Diagnostic {
                    run_id: None,
                    diagnostic,
                } => self.events.push(TestSessionEvent::Diagnostic {
                    run_id: None,
                    diagnostic,
                }),
                event => {
                    let run_id = self.watch_run_id.clone().ok_or_else(|| {
                        test_protocol_error("test host emitted a watch result outside a watch run")
                    })?;
                    self.record_event(&run_id, event)?;
                }
            },
            HostResponseBody::WatchRunError {
                watch_id,
                run_id,
                started,
                error,
            } => {
                if self.watch_id.as_deref() != Some(watch_id.as_str()) {
                    return Err(test_protocol_error("test host failed an unrelated watch"));
                }
                if started {
                    if self.watch_run_id.as_deref() != run_id.as_deref() || !self.watch_run_active {
                        return Err(test_protocol_error(
                            "test host failed an unrelated active watch run",
                        ));
                    }
                    self.watch_run_active = false;
                    self.watch_run_id = None;
                } else if run_id.is_some() || self.watch_run_active {
                    return Err(test_protocol_error(
                        "test host reported a pre-start watch failure during an active run",
                    ));
                }
                self.events.push(TestSessionEvent::Diagnostic {
                    run_id: run_id.clone(),
                    diagnostic: Box::new(TestDiagnostic {
                        severity: wake_test_contract::DiagnosticSeverity::Error,
                        code: error.code.clone(),
                        message: error.message.clone(),
                        path: error.path.clone(),
                        location: None,
                        notes: Vec::new(),
                    }),
                });
                if started {
                    self.watch_id = None;
                    self.watch_request_id = None;
                    self.events.push(TestSessionEvent::Closed);
                    self.close_transport();
                    return Err(wake_error_from_host(error));
                }
            }
            HostResponseBody::Error { .. } => {
                return Err(test_protocol_error(
                    "test host used a one-shot error frame on the watch stream",
                ));
            }
            _ => {
                return Err(test_protocol_error(
                    "test host emitted a non-event frame on the watch stream",
                ));
            }
        }
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<TestSessionEvent> {
        std::mem::take(&mut self.events)
    }

    fn send_command(&mut self, command: HostCommand) -> Result<u64, WakeError> {
        if self.closed {
            return Err(WakeError::closed("TestSession"));
        }
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| test_protocol_error("test-host request id overflowed"))?;
        let request = HostRequest {
            protocol_version: PROTOCOL_VERSION,
            build_id: HOST_BUILD_ID.to_string(),
            token: self.token.clone(),
            request_id: self.next_request_id,
            command,
        };
        write_frame(&mut self.writer, &request)
            .map_err(|error| test_protocol_error(format!("could not send request: {error}")))?;
        Ok(self.next_request_id)
    }

    fn receive_response(&mut self) -> Result<Option<HostResponse>, WakeError> {
        let response = match self.incoming.recv_timeout(TEST_HOST_POLL_INTERVAL) {
            Ok(TestProtocolIncoming::Response(response)) => *response,
            Ok(TestProtocolIncoming::Closed) => {
                return Err(test_protocol_error("test host closed the protocol session"));
            }
            Ok(TestProtocolIncoming::DecodeError(error)) => {
                return Err(test_protocol_error(format!(
                    "could not decode response: {error}"
                )));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(test_protocol_error(
                    "test-host response reader disconnected",
                ));
            }
        };
        self.accept_response(response).map(Some)
    }

    fn try_receive_response(&mut self) -> Result<Option<HostResponse>, WakeError> {
        let response = match self.incoming.try_recv() {
            Ok(TestProtocolIncoming::Response(response)) => *response,
            Ok(TestProtocolIncoming::Closed) => {
                return Err(test_protocol_error("test host closed the protocol session"));
            }
            Ok(TestProtocolIncoming::DecodeError(error)) => {
                return Err(test_protocol_error(format!(
                    "could not decode response: {error}"
                )));
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(test_protocol_error(
                    "test-host response reader disconnected",
                ));
            }
        };
        self.accept_response(response).map(Some)
    }

    fn accept_response(&mut self, response: HostResponse) -> Result<HostResponse, WakeError> {
        if response.protocol_version != PROTOCOL_VERSION || response.build_id != HOST_BUILD_ID {
            return Err(test_protocol_error(
                "test host returned a mismatched protocol/build envelope",
            ));
        }
        let expected_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| test_protocol_error("test-host response sequence overflowed"))?;
        if response.sequence != expected_sequence {
            return Err(test_protocol_error(format!(
                "test host response sequence {}, expected {}",
                response.sequence, expected_sequence
            )));
        }
        self.next_sequence = response.sequence;
        Ok(response)
    }

    fn await_control_ack(
        &mut self,
        request_id: u64,
        matches_ack: impl Fn(&HostAck) -> bool,
    ) -> Result<(), WakeError> {
        let started = Instant::now();
        loop {
            if started.elapsed() >= TEST_HOST_CONTROL_TIMEOUT {
                return Err(test_protocol_error(format!(
                    "test host did not acknowledge request {request_id} within 10 seconds"
                )));
            }
            let Some(response) = self.receive_response()? else {
                continue;
            };
            if self.watch_request_id == Some(response.request_id)
                && response.request_id != request_id
            {
                self.record_watch_response(response)?;
                continue;
            }
            if response.request_id != request_id {
                return Err(test_protocol_error(format!(
                    "response {} does not match control request {request_id}",
                    response.request_id
                )));
            }
            match response.body {
                HostResponseBody::Ack { command } if matches_ack(&command) => return Ok(()),
                HostResponseBody::Error { error, .. } => return Err(wake_error_from_host(error)),
                _ => {
                    return Err(test_protocol_error(
                        "test host returned the wrong control response",
                    ));
                }
            }
        }
    }

    fn record_event(&mut self, active_run: &str, event: HostEvent) -> Result<(), WakeError> {
        let event = match event {
            HostEvent::RunStart { run_id, watching } if run_id == active_run => {
                TestSessionEvent::RunStart { run_id, watching }
            }
            HostEvent::TestCaseResult {
                run_id,
                suite_id,
                result,
            } if run_id == active_run => TestSessionEvent::TestCaseResult {
                run_id,
                suite_id,
                result,
            },
            HostEvent::SuiteResult { run_id, result } if run_id == active_run => {
                TestSessionEvent::SuiteResult { run_id, result }
            }
            HostEvent::Diagnostic { run_id, diagnostic }
                if run_id.as_deref().is_none_or(|run_id| run_id == active_run) =>
            {
                TestSessionEvent::Diagnostic { run_id, diagnostic }
            }
            _ => {
                return Err(test_protocol_error(format!(
                    "test host emitted an event for a run other than `{active_run}`"
                )));
            }
        };
        self.events.push(event);
        Ok(())
    }

    fn close_transport(&mut self) {
        self.reader_stop.store(true, Ordering::Release);
        let _ = self.writer.shutdown(Shutdown::Both);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.closed = true;
    }
}

impl Drop for TestProtocolSession {
    fn drop(&mut self) {
        self.close_transport();
    }
}

fn read_test_protocol_frames(
    mut stream: TcpStream,
    sender: &mpsc::Sender<TestProtocolIncoming>,
    stop: &AtomicBool,
) {
    if let Err(error) = stream.set_read_timeout(Some(TEST_HOST_READER_POLL_INTERVAL)) {
        let _ = sender.send(TestProtocolIncoming::DecodeError(error.to_string()));
        return;
    }
    let mut decoder = FrameDecoder::new();
    let mut chunk = [0_u8; 16 * 1024];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                let incoming = if decoder.is_empty() {
                    TestProtocolIncoming::Closed
                } else {
                    TestProtocolIncoming::DecodeError(
                        "session ended in the middle of a frame".to_string(),
                    )
                };
                let _ = sender.send(incoming);
                return;
            }
            Ok(length) => {
                decoder.push(&chunk[..length]);
                loop {
                    match decoder.decode_next::<HostResponse>() {
                        Ok(Some(response)) => {
                            if sender
                                .send(TestProtocolIncoming::Response(Box::new(response)))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let _ =
                                sender.send(TestProtocolIncoming::DecodeError(error.to_string()));
                            return;
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                let _ = sender.send(TestProtocolIncoming::DecodeError(error.to_string()));
                return;
            }
        }
    }
}

fn test_protocol_error(message: impl Into<String>) -> WakeError {
    WakeError::new("WAKE_TEST_HOST", message)
}

fn wake_error_from_host(error: HostError) -> WakeError {
    let mut wake_error = WakeError::new(error.code, error.message);
    wake_error.path = error.path;
    wake_error
}

fn resolve_test_host(explicit: Option<&Path>) -> Result<PathBuf, WakeError> {
    let candidate = explicit
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("WAKE_TEST_HOST_PATH").map(PathBuf::from))
        .or_else(|| {
            let executable = std::env::current_exe().ok()?;
            Some(executable.with_file_name(if cfg!(windows) {
                "wake-test-host.exe"
            } else {
                "wake-test-host"
            }))
        })
        .ok_or_else(|| WakeError::new("WAKE_TEST_HOST", "could not resolve test host path"))?;
    if !candidate.is_file() {
        return Err(WakeError::new(
            "WAKE_TEST_HOST",
            "test host executable is missing; reinstall Wake or set WAKE_TEST_HOST_PATH",
        )
        .at(&candidate));
    }
    Ok(candidate)
}

fn test_host_token() -> Result<String, WakeError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| {
        WakeError::new(
            "WAKE_TEST_HOST",
            format!("could not create test-host authentication token: {error}"),
        )
    })?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(token)
}

fn execute_build(
    options: BuildOptions,
    cancellation: &CancellationToken,
    project_defaults: bool,
) -> Result<BuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let prepared = prepare_build(&options)?;
    cancellation.check()?;
    let lifetime = if options.cache {
        BundlerLifetime::Session
    } else {
        BundlerLifetime::OneShot
    };
    let mut bundler = create_bundler(&prepared, &options, project_defaults, lifetime)?;
    let output = bundler.build(&prepared.entry);
    cancellation.check()?;
    finish_output(
        &prepared,
        &options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
    )
}

fn execute_bundle(
    options: BundleOptions,
    cancellation: &CancellationToken,
) -> Result<BundleResult, WakeError> {
    let timing = std::env::var_os("WAKE_TIMING").is_some();
    let started = Instant::now();
    let phase_started = Instant::now();
    let options = resolve_bundle_options(options)?;
    let resolve_options_elapsed = phase_started.elapsed();
    cancellation.check()?;
    let phase_started = Instant::now();
    let prepared = prepare_build(&BuildOptions {
        project: options.project.clone(),
        entry: options.entry.clone(),
        write: false,
        ..BuildOptions::default()
    })?;
    let prepare_elapsed = phase_started.elapsed();
    let lifetime = if options.cache {
        BundlerLifetime::Session
    } else {
        BundlerLifetime::OneShot
    };
    let phase_started = Instant::now();
    let mut bundler = create_bundle_bundler(&prepared, &options, lifetime)?;
    let create_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
    let output = bundler.build(&prepared.entry);
    cancellation.check()?;
    let build_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
    let mut result = finish_bundle(
        &prepared,
        &options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
    )?;
    let finish_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
    drop(bundler);
    let drop_elapsed = phase_started.elapsed();
    let total_elapsed = started.elapsed();
    result.duration_ms = total_elapsed.as_secs_f64() * 1000.0;
    if timing {
        eprintln!(
            "[wake-app-timing] resolve-options={resolve_options_elapsed:.1?} prepare={prepare_elapsed:.1?} create={create_elapsed:.1?} build={build_elapsed:.1?} finish={finish_elapsed:.1?} drop={drop_elapsed:.1?} total={total_elapsed:.1?}",
        );
    }
    Ok(result)
}

fn resolve_bundle_options(options: BundleOptions) -> Result<ResolvedBundleOptions, WakeError> {
    let platform = options.platform.unwrap_or(BuildPlatform::Browser);
    let format = options.format.unwrap_or(match platform {
        BuildPlatform::Browser => ModuleFormat::Iife,
        BuildPlatform::Node => ModuleFormat::CommonJs,
    });
    let valid_pair = matches!(
        (platform, format),
        (BuildPlatform::Browser, ModuleFormat::Iife)
            | (BuildPlatform::Node, ModuleFormat::CommonJs)
    );
    if !valid_pair {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "supported bundle combinations are browser+iife and node+cjs",
        ));
    }
    if platform == BuildPlatform::Browser && options.target.is_some() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "explicit target is currently only supported for Node bundles",
        ));
    }
    for package in &options.external {
        if !is_bare_package_name(package) {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!("external must be a bare package name: {package}"),
            ));
        }
    }
    Ok(ResolvedBundleOptions {
        project: options.project,
        entry: options.entry,
        outfile: options.outfile,
        platform,
        format,
        target: (platform == BuildPlatform::Node)
            .then(|| options.target.unwrap_or_else(|| "node20".to_string())),
        external: options.external,
        minify: options.minify,
        source_map: options.source_map,
        cache: options.cache,
    })
}

fn is_bare_package_name(value: &str) -> bool {
    if value.is_empty()
        || value.starts_with('.')
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains('*')
        || value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || value
            .chars()
            .any(|character| matches!(character, ':' | '#' | '?' | '%'))
    {
        return false;
    }
    if value.starts_with('@') {
        let mut parts = value.split('/');
        return parts
            .next()
            .is_some_and(|scope| valid_package_part(&scope[1..]))
            && parts.next().is_some_and(valid_package_part)
            && parts.next().is_none();
    }
    !value.contains('/') && valid_package_part(value)
}

fn valid_package_part(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.starts_with('.')
}

fn prepare_build(options: &BuildOptions) -> Result<PreparedBuild, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    let configured_root = normalize_path(&config.resolved_root(&config_dir));
    if !configured_root.is_dir() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "configured project root does not exist: {}",
                configured_root.display()
            ),
        )
        .at(&configured_root));
    }
    let root = canonical_project_root(&configured_root)?;
    let aliases = prepare_aliases_and_scans(&config, &root)?;
    let entry = match &options.entry {
        Some(entry) => absolute_from(&root, entry),
        None => virtual_entry(&root, &config)?,
    };
    if !entry.is_file() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!("entry file does not exist: {}", entry.display()),
        )
        .at(&entry));
    }
    let entry = entry
        .canonicalize()
        .map(|entry| wake_common::fs::normalize(&entry))
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&entry))?;
    let outdir = absolute_from(
        &root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("dist")),
    );
    Ok(PreparedBuild {
        root,
        entry,
        outdir,
        config,
        aliases,
    })
}

fn resolve_config_dir(cwd: &Path, config_path: Option<&Path>) -> Result<PathBuf, WakeError> {
    let cwd = if cwd.is_absolute() {
        normalize_path(cwd)
    } else {
        let process_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        normalize_path(&process_cwd.join(cwd))
    };
    if !cwd.is_dir() {
        return Err(
            WakeError::new("WAKE_CONFIG", "cwd does not exist or is not a directory").at(&cwd),
        );
    }
    if let Some(config_path) = config_path {
        let path = absolute_from(&cwd, config_path);
        if path.file_name().and_then(|name| name.to_str()) != Some(wake_config::CONFIG_FILE) {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!("configPath must point to {}", wake_config::CONFIG_FILE),
            )
            .at(&path));
        }
        if !path.is_file() {
            return Err(
                WakeError::new("WAKE_CONFIG", "configuration file does not exist").at(&path),
            );
        }
        return Ok(path.parent().unwrap_or(&cwd).to_path_buf());
    }
    Ok(wake_config::find_root(&cwd))
}

fn prepare_aliases_and_scans(
    config: &wake_config::Config,
    root: &Path,
) -> Result<Vec<(String, PathBuf)>, WakeError> {
    let mut aliases = config.resolver_aliases(root);
    if config.component_scan.is_empty() {
        return Ok(aliases);
    }
    let scan_base = root.join(".wake").join("scan");
    std::fs::create_dir_all(&scan_base)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&scan_base))?;
    for rule in &config.component_scan {
        let source = wake_scan::scan(&wake_scan::ScanRule {
            namespace: &rule.namespace,
            scan_dir: &root.join(&rule.cwd),
            root,
            generate_source: rule.generate_source,
            include: rule.include.as_deref(),
            exclude: rule.exclude.as_deref(),
        })
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
        let file = scan_base.join(format!("{}.ts", sanitize_namespace(&rule.namespace)));
        std::fs::write(&file, source)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&file))?;
        aliases.push((format!("@@@/{}", rule.namespace), file));
    }
    Ok(aliases)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BundlerLifetime {
    OneShot,
    Session,
}

fn create_bundler(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
    lifetime: BundlerLifetime,
) -> Result<IncrementalBundler, WakeError> {
    let mut bundler = match lifetime {
        BundlerLifetime::OneShot => IncrementalBundler::new_one_shot(Arc::new(OsFileSystem)),
        BundlerLifetime::Session => IncrementalBundler::new(Arc::new(OsFileSystem)),
    };
    bundler.set_project_root(prepared.root.clone());
    bundler.set_resolve_options(ResolveOptions {
        alias: prepared.aliases.clone(),
        ..ResolveOptions::default()
    });
    bundler.set_define(build_defines(&prepared.config, !project_defaults));
    bundler.set_target_env(resolve_target_env(&prepared.config, &prepared.root)?);
    bundler.set_jsx_runtime(
        false,
        Box::leak(
            prepared
                .config
                .react
                .jsx_import_source
                .clone()
                .into_boxed_str(),
        ),
    );
    bundler.enable_css_in_js();
    bundler.set_asset_inline_limit(4096);
    bundler.set_public_path(prepared.config.public_path());
    if project_defaults {
        bundler.enable_css_extraction();
        bundler.enable_dead_module_elimination();
        bundler.enable_tree_shaking();
        bundler.enable_minify();
        bundler.enable_code_splitting();
        if options.source_map {
            bundler.enable_sourcemap();
        }
    } else if options.source_map {
        bundler.enable_sourcemap();
    }
    if options.cache {
        let cache_dir = prepared.root.join(".wake");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&cache_dir))?;
        bundler.enable_persistent_cache(cache_dir.join("cache.bin"));
    }
    Ok(bundler)
}

fn create_bundle_bundler(
    prepared: &PreparedBuild,
    options: &ResolvedBundleOptions,
    lifetime: BundlerLifetime,
) -> Result<IncrementalBundler, WakeError> {
    let mut bundler = match lifetime {
        BundlerLifetime::OneShot => IncrementalBundler::new_one_shot(Arc::new(OsFileSystem)),
        BundlerLifetime::Session => IncrementalBundler::new(Arc::new(OsFileSystem)),
    };
    bundler
        .set_project_root(prepared.root.clone())
        .set_resolve_options(ResolveOptions {
            alias: prepared.aliases.clone(),
            ..ResolveOptions::default()
        })
        .set_platform(options.platform)
        .set_module_format(options.format)
        .set_external_packages(options.external.clone())
        .set_define(build_defines(
            &prepared.config,
            options.platform == BuildPlatform::Browser,
        ))
        .set_jsx_runtime(
            false,
            Box::leak(
                prepared
                    .config
                    .react
                    .jsx_import_source
                    .clone()
                    .into_boxed_str(),
            ),
        )
        .set_content_hash(false);

    if options.platform == BuildPlatform::Browser {
        bundler.enable_css_in_js();
        bundler.set_public_path(prepared.config.public_path());
        // 省略 outfile 时保持旧的内存 bundle 资源阈值；精确 outfile 是严格单文件，
        // 因此必须内联资源，不能在目标目录旁静默生成额外文件。
        bundler.set_asset_inline_limit(if options.outfile.is_some() {
            usize::MAX
        } else {
            4096
        });
    }

    let target = match options.platform {
        BuildPlatform::Browser => resolve_target_env(&prepared.config, &prepared.root)?,
        BuildPlatform::Node => node_target_env(
            options
                .target
                .as_deref()
                .expect("Node bundle target is normalized"),
        )?,
    };
    bundler.set_target_env(target);
    if options.minify {
        bundler
            .enable_minify()
            .enable_tree_shaking()
            .enable_dead_module_elimination();
    }
    if options.source_map {
        bundler.enable_sourcemap();
    }
    if options.cache {
        let cache_dir = prepared.root.join(".wake");
        std::fs::create_dir_all(&cache_dir)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&cache_dir))?;
        bundler.enable_persistent_cache(cache_dir.join("bundle-cache.bin"));
    }
    Ok(bundler)
}

fn node_target_env(target: &str) -> Result<TargetEnv, WakeError> {
    let version = target.strip_prefix("node").unwrap_or("");
    let valid = !version.is_empty()
        && version.split('.').count() <= 2
        && version.split('.').all(|component| {
            !component.is_empty() && component.chars().all(|c| c.is_ascii_digit())
        });
    if !valid {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Node target `{target}`; expected node20 or node20.0"),
        ));
    }
    Ok(TargetEnv::new(vec![BrowserTarget::new("node", version)]))
}

fn create_session(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
) -> Result<BuildSession, WakeError> {
    Ok(BuildSession::from_incremental(create_bundler(
        prepared,
        options,
        project_defaults,
        BundlerLifetime::Session,
    )?))
}

fn finish_output(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    output: BuildOutput,
    duration_ms: f64,
) -> Result<BuildResult, WakeError> {
    let diagnostics = diagnostic_infos(&output.diagnostics, &prepared.root);
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake build failed").with_diagnostic_infos(diagnostics)
        );
    }

    let mut files = output
        .chunks
        .iter()
        .map(|chunk| OutputFile {
            path: chunk.file_name.clone(),
            kind: "chunk".to_string(),
            bytes: chunk.code.len(),
        })
        .chain(output.assets.iter().map(|asset| OutputFile {
            path: asset.file_name.clone(),
            kind: if asset.is_css { "css" } else { "asset" }.to_string(),
            bytes: asset.bytes.len(),
        }))
        .collect::<Vec<_>>();

    let output_dir = if options.write {
        write_build_output(&output, &prepared.outdir)?;
        let html = emit_html(&output, &prepared.config);
        let html_path = prepared.outdir.join("index.html");
        std::fs::write(&html_path, html.as_bytes())
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&html_path))?;
        files.push(OutputFile {
            path: "index.html".to_string(),
            kind: "html".to_string(),
            bytes: html.len(),
        });
        Some(prepared.outdir.to_string_lossy().into_owned())
    } else {
        None
    };

    Ok(BuildResult {
        success: true,
        module_count: output.module_count,
        updated_module_count: output.updated_module_count,
        cached_module_count: output.cached_module_count,
        duration_ms,
        output_dir,
        code: (!options.write).then(|| output.bundle.clone()),
        files,
        diagnostics,
    })
}

fn finish_bundle(
    prepared: &PreparedBuild,
    options: &ResolvedBundleOptions,
    output: BuildOutput,
    duration_ms: f64,
) -> Result<BundleResult, WakeError> {
    let diagnostics = diagnostic_infos(&output.diagnostics, &prepared.root);
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake bundle failed").with_diagnostic_infos(diagnostics)
        );
    }
    if options.platform == BuildPlatform::Node && !output.assets.is_empty() {
        return Err(WakeError::new(
            "WAKE_BUILD",
            "Node bundle cannot emit browser assets",
        ));
    }
    if options.outfile.is_some() && !output.assets.is_empty() {
        return Err(WakeError::new(
            "WAKE_BUILD",
            "single-file bundle cannot emit sibling assets",
        ));
    }

    let output_file = options
        .outfile
        .as_deref()
        .map(|outfile| absolute_from(&prepared.root, outfile));
    let mut source_map = output.entry().source_map.clone();
    if let (Some(map), Some(output_path)) = (&source_map, &output_file)
        && let Some(file_name) = output_path.file_name().and_then(|name| name.to_str())
    {
        source_map = Some(rewrite_source_map_file(map, file_name)?);
    }
    let source_map_file = output_file
        .as_ref()
        .filter(|_| source_map.is_some())
        .map(|path| append_path_suffix(path, ".map"));
    let mut code = output.bundle.clone();
    if let Some(map_path) = &source_map_file
        && let Some(map_name) = map_path.file_name().and_then(|name| name.to_str())
    {
        code.push_str("//# sourceMappingURL=");
        code.push_str(map_name);
        code.push('\n');
    }
    if let Some(path) = &output_file {
        if let (Some(map), Some(map_path)) = (&source_map, &source_map_file) {
            atomic_write(map_path, map.as_bytes())?;
        }
        atomic_write(path, code.as_bytes())?;
    }
    let mut files = vec![OutputFile {
        path: output_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| output.entry().file_name.clone()),
        kind: "chunk".to_string(),
        bytes: code.len(),
    }];
    if output_file.is_none() {
        files.extend(output.assets.iter().map(|asset| OutputFile {
            path: asset.file_name.clone(),
            kind: if asset.is_css { "css" } else { "asset" }.to_string(),
            bytes: asset.bytes.len(),
        }));
    }
    if let Some(map) = &source_map {
        files.push(OutputFile {
            path: source_map_file
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.map", output.entry().file_name)),
            kind: "map".to_string(),
            bytes: map.len(),
        });
    }

    Ok(BundleResult {
        success: true,
        module_count: output.module_count,
        updated_module_count: output.updated_module_count,
        cached_module_count: output.cached_module_count,
        duration_ms,
        output_file: output_file.map(|path| path.to_string_lossy().into_owned()),
        code,
        source_map,
        source_map_file: source_map_file.map(|path| path.to_string_lossy().into_owned()),
        files,
        diagnostics,
    })
}

fn rewrite_source_map_file(map: &str, file_name: &str) -> Result<String, WakeError> {
    let mut value = serde_json::from_str::<serde_json::Value>(map).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("Wake generated an invalid source map: {error}"),
        )
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        WakeError::new(
            "WAKE_INTERNAL",
            "Wake generated a source map whose root is not an object",
        )
    })?;
    object.insert(
        "file".to_string(),
        serde_json::Value::String(file_name.to_string()),
    );
    serde_json::to_string(&value).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("Wake could not serialize its source map: {error}"),
        )
    })
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), WakeError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent))?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".wake-bundle-")
        .tempfile_in(parent)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent))?;
    temporary
        .write_all(bytes)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(path))?;
    for attempt in 0..10_u64 {
        match temporary.persist(path) {
            Ok(_) => return Ok(()),
            Err(error) => {
                let retryable = matches!(
                    error.error.kind(),
                    std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::AlreadyExists
                );
                if !retryable || attempt == 9 {
                    return Err(WakeError::new("WAKE_IO", error.error.to_string()).at(path));
                }
                temporary = error.file;
                thread::sleep(std::time::Duration::from_millis((attempt + 1).min(5)));
            }
        }
    }
    unreachable!("atomic write retry loop returns on every terminal state")
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn write_build_output(output: &BuildOutput, outdir: &Path) -> Result<(), WakeError> {
    clean_outdir(outdir)?;
    std::fs::create_dir_all(outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(outdir))?;
    for chunk in &output.chunks {
        let path = outdir.join(&chunk.file_name);
        std::fs::write(&path, &chunk.code)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
        if let Some(map) = &chunk.source_map {
            let map_path = outdir.join(format!("{}.map", chunk.file_name));
            std::fs::write(&map_path, map)
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&map_path))?;
        }
    }
    for asset in &output.assets {
        let path = outdir.join(&asset.file_name);
        std::fs::write(&path, &asset.bytes)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    }
    let manifest = serde_json::json!({
        "entry": output.entry().file_name,
        "chunks": output.chunks.iter().map(|chunk| &chunk.file_name).collect::<Vec<_>>(),
        "chunkStyles": output.chunks.iter().map(|chunk| serde_json::json!({
            "chunk": &chunk.file_name,
            "styles": &chunk.styles,
        })).collect::<Vec<_>>(),
        "assets": output.assets.iter().map(|asset| &asset.file_name).collect::<Vec<_>>(),
    });
    let path = outdir.join("manifest.json");
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("manifest serialization"),
    )
    .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    Ok(())
}

fn emit_html(output: &BuildOutput, config: &wake_config::Config) -> String {
    let scripts = output
        .chunks
        .iter()
        .filter(|chunk| chunk.is_entry)
        .map(|chunk| chunk.file_name.clone())
        .collect::<Vec<_>>();
    let styles = output.entry().styles.clone();
    wake_html::generate(
        None,
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: config.public_path(),
        },
    )
}
fn build_defines(config: &wake_config::Config, development: bool) -> Vec<(String, String)> {
    let node_env = if development {
        "\"development\""
    } else {
        "\"production\""
    };
    let mut values = vec![
        ("process.env.NODE_ENV".to_string(), node_env.to_string()),
        // Wake emits classic-script chunks and currently provides live reload rather than
        // a module-level HMR API. Do not leak this ESM-only syntax into those chunks.
        ("import.meta.hot".to_string(), "false".to_string()),
        (
            "import.meta.url".to_string(),
            "__wake_require__.metaUrl()".to_string(),
        ),
    ];
    for (key, value) in &config.define {
        if let Some(existing) = values.iter_mut().find(|(name, _)| name == key) {
            existing.1 = value.clone();
        } else {
            values.push((key.clone(), value.clone()));
        }
    }
    values
}

fn resolve_target_env(config: &wake_config::Config, root: &Path) -> Result<TargetEnv, WakeError> {
    let targets = config
        .resolve_browser_targets(root)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(root))?
        .into_iter()
        .map(|target| BrowserTarget::new(target.name, target.version))
        .collect();
    let mut environment = TargetEnv::new(targets);
    environment
        .apply_overrides(&config.transforms.include, &config.transforms.exclude)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error))?;
    Ok(environment)
}

fn virtual_entry(root: &Path, config: &wake_config::Config) -> Result<PathBuf, WakeError> {
    let target = config
        .html
        .entry
        .as_deref()
        .unwrap_or("src/entry.tsx")
        .replace('\\', "/");
    let dir = root.join(".wake");
    std::fs::create_dir_all(&dir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&dir))?;
    let path = dir.join("entry.tsx");
    std::fs::write(&path, format!("import(\"@@/{target}\");\n"))
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    Ok(path)
}

fn clean_outdir(outdir: &Path) -> Result<(), WakeError> {
    if outdir.exists() {
        if outdir.file_name().is_none() || outdir == Path::new(".") {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!(
                    "refusing to clean unsafe output directory: {}",
                    outdir.display()
                ),
            ));
        }
        std::fs::remove_dir_all(outdir)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(outdir))?;
    }
    Ok(())
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&root.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                output.pop();
            }
            Component::CurDir => {}
            other => output.push(other.as_os_str()),
        }
    }
    output
}

/// Resolve one stable physical identity for project-local paths before aliases, entries, caches,
/// and file watchers are created. On Windows this expands 8.3 paths such as `RUNNER~1`; without
/// it, notify can report a long path that does not match the bundler's short-path cache key.
fn canonical_project_root(path: &Path) -> Result<PathBuf, WakeError> {
    path.canonicalize()
        .map(|path| wake_common::fs::normalize(&path))
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(path))
}

fn sanitize_namespace(namespace: &str) -> String {
    namespace
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect()
}

enum ContextCommand {
    Rebuild {
        changed_paths: Vec<PathBuf>,
        cancellation: CancellationToken,
        response: mpsc::Sender<Result<BuildResult, WakeError>>,
    },
    Close {
        response: mpsc::Sender<()>,
    },
}

struct BuildContextInner {
    sender: mpsc::Sender<ContextCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct BuildContext {
    inner: Arc<BuildContextInner>,
}

impl BuildContext {
    pub fn create(options: BuildOptions) -> Result<Self, WakeError> {
        let prepared = prepare_build(&options)?;
        let (sender, receiver) = mpsc::channel();
        let join = thread::Builder::new()
            .name("wake-build-context".to_string())
            .spawn(move || {
                let session = create_session(&prepared, &options, true);
                run_build_context(receiver, prepared, options, session);
            })
            .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?;
        Ok(Self {
            inner: Arc::new(BuildContextInner {
                sender,
                join: Mutex::new(Some(join)),
                closed: AtomicBool::new(false),
            }),
        })
    }

    pub fn rebuild(
        &self,
        changed_paths: Vec<PathBuf>,
        cancellation: CancellationToken,
    ) -> Result<BuildResult, WakeError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WakeError::closed("BuildContext"));
        }
        let (sender, receiver) = mpsc::channel();
        self.inner
            .sender
            .send(ContextCommand::Rebuild {
                changed_paths,
                cancellation,
                response: sender,
            })
            .map_err(|_| WakeError::closed("BuildContext"))?;
        receiver
            .recv()
            .map_err(|_| WakeError::closed("BuildContext"))?
    }

    pub fn request_close(&self) {
        if !self.inner.closed.swap(true, Ordering::AcqRel) {
            let (sender, _) = mpsc::channel();
            let _ = self
                .inner
                .sender
                .send(ContextCommand::Close { response: sender });
        }
    }

    pub fn close(&self) {
        let response = if self.inner.closed.swap(true, Ordering::AcqRel) {
            None
        } else {
            let (sender, receiver) = mpsc::channel();
            let _ = self
                .inner
                .sender
                .send(ContextCommand::Close { response: sender });
            Some(receiver)
        };
        if let Some(receiver) = response {
            let _ = receiver.recv();
        }
        let mut join = self
            .inner
            .join
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(join) = join.take() {
            let _ = join.join();
        }
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }
}

impl Drop for BuildContextInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let (sender, _) = mpsc::channel();
        let _ = self.sender.send(ContextCommand::Close { response: sender });
    }
}

fn run_build_context(
    receiver: mpsc::Receiver<ContextCommand>,
    prepared: PreparedBuild,
    options: BuildOptions,
    session: Result<BuildSession, WakeError>,
) {
    let mut session = match session {
        Ok(session) => session,
        Err(error) => {
            while let Ok(command) = receiver.recv() {
                match command {
                    ContextCommand::Rebuild { response, .. } => {
                        let _ = response.send(Err(error.clone()));
                    }
                    ContextCommand::Close { response } => {
                        let _ = response.send(());
                        break;
                    }
                }
            }
            return;
        }
    };
    while let Ok(command) = receiver.recv() {
        match command {
            ContextCommand::Rebuild {
                changed_paths,
                cancellation,
                response,
            } => {
                let result = if let Err(error) = cancellation.check() {
                    Err(error)
                } else {
                    let started = Instant::now();
                    let mut paths = changed_paths
                        .iter()
                        .map(|path| absolute_from(&prepared.root, path))
                        .collect::<Vec<_>>();
                    match prepare_aliases_and_scans(&prepared.config, &prepared.root) {
                        Ok(aliases) => {
                            paths.extend(aliases.into_iter().filter_map(|(name, path)| {
                                name.starts_with("@@@/").then_some(path)
                            }))
                        }
                        Err(error) => {
                            let _ = response.send(Err(error));
                            continue;
                        }
                    }
                    paths.sort();
                    paths.dedup();
                    if paths.is_empty() {
                        session.invalidate_filesystem();
                    } else {
                        session.invalidate_paths(&paths, true);
                    }
                    let output = session.build_current(BuildRequest::new(&prepared.entry));
                    cancellation.check().and_then(|()| {
                        finish_output(
                            &prepared,
                            &options,
                            output,
                            started.elapsed().as_secs_f64() * 1000.0,
                        )
                    })
                };
                let _ = response.send(result);
            }
            ContextCommand::Close { response } => {
                let _ = response.send(());
                break;
            }
        }
    }
}
#[derive(Debug, Clone, Default)]
pub struct DevServerOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub open: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DevServerEvent {
    RebuildStart {
        changed_paths: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
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
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        base_path: Option<String>,
    },
    Diagnostic {
        diagnostic: DiagnosticInfo,
    },
    WorkspaceState {
        total: usize,
        loaded: usize,
        failed: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        failed_names: Vec<String>,
    },
    Closed,
}

#[derive(Clone)]
pub struct DevServer {
    handle: wake_dev_server::ServerHandle,
    events: Arc<Mutex<mpsc::Receiver<DevServerEvent>>>,
}

impl DevServer {
    pub fn url(&self) -> &str {
        self.handle.url()
    }

    pub fn request_close(&self) {
        self.handle.request_close();
    }

    pub fn close(&self) -> Result<(), WakeError> {
        self.handle
            .close()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))
    }

    pub fn wait_until_closed(&self) -> Result<(), WakeError> {
        self.handle
            .wait()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))
    }
    pub fn drain_events(&self) -> Vec<DevServerEvent> {
        let receiver = self
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        receiver.try_iter().collect()
    }
}

fn forward_dev_server_event(
    sender: &mpsc::Sender<DevServerEvent>,
    root: &Path,
    event: wake_dev_server::ServerEvent,
) {
    match event {
        wake_dev_server::ServerEvent::RebuildStart {
            changed_paths,
            workspace,
            base_path,
        } => {
            let _ = sender.send(DevServerEvent::RebuildStart {
                changed_paths: changed_paths
                    .into_iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect(),
                workspace,
                base_path,
            });
        }
        wake_dev_server::ServerEvent::Rebuilt {
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
            let _ = sender.send(DevServerEvent::Rebuilt {
                initial,
                modules,
                updated_modules,
                cached_modules,
                chunks,
                assets,
                duration_ms,
                workspace,
                base_path,
            });
        }
        wake_dev_server::ServerEvent::Diagnostics { diagnostics } => {
            for diagnostic in diagnostic_infos(&diagnostics, root) {
                let _ = sender.send(DevServerEvent::Diagnostic { diagnostic });
            }
        }
        wake_dev_server::ServerEvent::WorkspaceState {
            total,
            loaded,
            failed,
            current,
            failed_names,
        } => {
            let _ = sender.send(DevServerEvent::WorkspaceState {
                total,
                loaded,
                failed,
                current,
                failed_names,
            });
        }
        wake_dev_server::ServerEvent::Closed => {
            let _ = sender.send(DevServerEvent::Closed);
        }
    }
}

pub fn start_dev_server(options: DevServerOptions) -> Result<DevServer, WakeError> {
    let build_options = BuildOptions {
        project: options.project,
        entry: options.entry,
        write: false,
        ..BuildOptions::default()
    };
    let prepared = prepare_build(&build_options)?;
    let config = &prepared.config;
    let server = &config.dev_server;
    let port = options.port.or(server.port).unwrap_or(5173);
    let host = options
        .host
        .or_else(|| server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let proxies = server
        .proxy
        .iter()
        .map(|proxy| wake_dev_server::ProxyRule {
            context: proxy.context.clone(),
            target: proxy.target.clone(),
            path_rewrite: proxy
                .path_rewrite
                .iter()
                .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                .collect(),
            change_origin: proxy.change_origin,
        })
        .collect();
    let watch_roots = config
        .component_scan
        .iter()
        .map(|rule| prepared.root.join(&rule.cwd))
        .collect();
    let scan_root = prepared.root.clone();
    let scan_config = config.clone();
    let before_rebuild: wake_dev_server::BeforeRebuild = Arc::new(move |_| {
        prepare_aliases_and_scans(&scan_config, &scan_root)
            .map(|aliases| {
                aliases
                    .into_iter()
                    .filter_map(|(name, path)| name.starts_with("@@@/").then_some(path))
                    .collect()
            })
            .map_err(|error| error.to_string())
    });
    let (event_tx, event_rx) = mpsc::channel();
    let event_root = prepared.root.clone();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        forward_dev_server_event(&event_tx, &event_root, event);
    });
    let serve_options = wake_dev_server::ServeOptions {
        entry: prepared.entry,
        base_path: config.public_path().to_string(),
        resolve_options: ResolveOptions {
            alias: prepared.aliases,
            conditions: ["browser", "development", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..ResolveOptions::default()
        },
        define: build_defines(config, true),
        host,
        open: options.open.unwrap_or(server.open),
        proxy: proxies,
        target_env: resolve_target_env(config, &prepared.root)?,
        jsx_import_source: config.react.jsx_import_source.clone(),
        watch_roots,
        before_rebuild: Some(before_rebuild),
        quiet: true,
        event_handler: Some(event_handler),
        mounts: Vec::new(),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
    })
}

#[derive(Debug, Clone, Default)]
pub struct DocsBuildOptions {
    pub project: ProjectOptions,
    pub outdir: Option<PathBuf>,
    pub base_path: Option<String>,
    pub presentation: Option<DocsPresentation>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocsWorkspaceBuildInfo {
    pub name: String,
    pub root: String,
    pub base_path: String,
    pub mode: DocsMode,
    pub presentation: String,
    pub demos: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocsBuildResult {
    #[serde(flatten)]
    pub build: BuildResult,
    pub routes: Vec<wake_docs::RouteInfo>,
    pub mode: DocsMode,
    pub demos: Vec<wake_docs::DemoDescriptor>,
    pub workspaces: Vec<DocsWorkspaceBuildInfo>,
}

#[derive(Debug, Clone)]
struct ResolvedDocsWorkspace {
    name: String,
    config_dir: PathBuf,
    root: PathBuf,
    base_path: String,
    presentation: wake_config::DocsWorkspacePresentation,
    dev_loading: wake_config::DocsWorkspaceDevLoading,
}

fn discover_docs_workspaces(
    options: &DocsBuildOptions,
) -> Result<Vec<ResolvedDocsWorkspace>, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    if config.docs.workspace.is_empty() {
        return Ok(Vec::new());
    }
    let configured_root = normalize_path(&config.resolved_root(&config_dir));
    let site_root = canonical_project_root(&configured_root)?;
    let site_base = normalize_public_path(
        options
            .base_path
            .as_deref()
            .unwrap_or(&config.docs.base_path),
    );
    let mut discovered = Vec::new();
    let mut seen_names = BTreeSet::new();
    let mut seen_roots = BTreeSet::new();
    let mut seen_bases = BTreeSet::new();

    for rule in &config.docs.workspace {
        validate_docs_workspace_rule(rule)?;
        let parent = absolute_from(&site_root, Path::new(&rule.root));
        let parent = canonical_project_root(&parent).map_err(|error| {
            WakeError::new(
                "WAKE_CONFIG",
                format!(
                    "cannot discover Docs workspaces below `{}`: {}",
                    parent.display(),
                    error.message
                ),
            )
            .at(&parent)
        })?;
        if !parent.is_dir() {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!(
                    "Docs workspace root is not a directory: {}",
                    parent.display()
                ),
            )
            .at(&parent));
        }
        let mut children = std::fs::read_dir(&parent)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&parent))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&parent))?;
        children.sort_by_key(std::fs::DirEntry::file_name);

        for child in children {
            let Some(name) = child.file_name().to_str().map(str::to_string) else {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!(
                        "Docs workspace name below `{}` is not valid UTF-8",
                        parent.display()
                    ),
                ));
            };
            if !rule
                .include
                .iter()
                .any(|pattern| wildcard_name_match(&name, pattern))
            {
                continue;
            }
            validate_workspace_name(&name)?;
            let child_path = child.path();
            if !child_path.is_dir() || !child_path.join(wake_config::CONFIG_FILE).is_file() {
                continue;
            }
            let child_root = canonical_project_root(&child_path)?;
            if !child_root.starts_with(&parent) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("Docs workspace `{name}` resolves outside its configured parent"),
                )
                .at(&child_path));
            }
            let child_config = wake_config::load(&child_root).map_err(|error| {
                WakeError::new(
                    "WAKE_CONFIG",
                    format!("Docs workspace `{name}` configuration is invalid: {error}"),
                )
                .at(&child_root)
            })?;
            let resolved_root =
                canonical_project_root(&normalize_path(&child_config.resolved_root(&child_root)))?;
            if !resolved_root.starts_with(&child_root) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("Docs workspace `{name}` root_dir escapes its workspace"),
                )
                .at(&resolved_root));
            }
            let base_path = effective_workspace_base(&site_base, &rule.base_path, &name)?;
            if !seen_names.insert(name.clone()) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("duplicate Docs workspace name `{name}`"),
                )
                .at(&resolved_root));
            }
            if !seen_roots.insert(resolved_root.clone()) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("Docs workspace `{name}` was discovered more than once"),
                )
                .at(&resolved_root));
            }
            if !seen_bases.insert(base_path.clone()) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!("duplicate Docs workspace base path `{base_path}`"),
                ));
            }
            discovered.push(ResolvedDocsWorkspace {
                name,
                config_dir: child_root,
                root: resolved_root,
                base_path,
                presentation: rule.presentation,
                dev_loading: rule.dev_loading,
            });
        }
    }

    discovered.sort_by(|left, right| left.name.cmp(&right.name));
    for (index, workspace) in discovered.iter().enumerate() {
        for other in &discovered[index + 1..] {
            if workspace.base_path.starts_with(&other.base_path)
                || other.base_path.starts_with(&workspace.base_path)
            {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!(
                        "overlapping Docs workspace base paths `{}` and `{}`",
                        workspace.base_path, other.base_path
                    ),
                ));
            }
        }
    }
    Ok(discovered)
}

fn validate_docs_workspace_rule(rule: &wake_config::DocsWorkspace) -> Result<(), WakeError> {
    if rule.root.trim().is_empty() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "Docs workspace root must not be empty",
        ));
    }
    if rule.include.is_empty() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "Docs workspace include must contain at least one name pattern",
        ));
    }
    for pattern in &rule.include {
        if pattern.is_empty()
            || pattern.chars().any(|character| {
                !(character.is_ascii_alphanumeric() || "._-*?".contains(character))
            })
        {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!("invalid Docs workspace include pattern `{pattern}`"),
            ));
        }
    }
    if !rule.base_path.contains("{name}") {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "Docs workspace base_path must contain `{name}`",
        ));
    }
    Ok(())
}

fn validate_workspace_name(name: &str) -> Result<(), WakeError> {
    let mut characters = name.chars();
    if !characters
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric())
        || characters.any(|character| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-'))
        })
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("Docs workspace name `{name}` is not a URL-safe path segment"),
        ));
    }
    Ok(())
}

fn wildcard_name_match(value: &str, pattern: &str) -> bool {
    let value = value.as_bytes();
    let pattern = pattern.as_bytes();
    let mut previous = vec![false; value.len() + 1];
    previous[0] = true;
    for token in pattern {
        let mut next = vec![false; value.len() + 1];
        if *token == b'*' {
            next[0] = previous[0];
            for index in 1..=value.len() {
                next[index] = previous[index] || next[index - 1];
            }
        } else {
            for index in 1..=value.len() {
                next[index] = previous[index - 1] && (*token == b'?' || *token == value[index - 1]);
            }
        }
        previous = next;
    }
    previous[value.len()]
}

fn effective_workspace_base(
    site_base: &str,
    template: &str,
    name: &str,
) -> Result<String, WakeError> {
    if !site_base.starts_with('/')
        || site_base
            .chars()
            .any(|character| matches!(character, '\\' | '%' | '?' | '#'))
        || site_base
            .trim_matches('/')
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Docs site base path `{site_base}`"),
        ));
    }
    if !template.starts_with('/')
        || template
            .chars()
            .any(|character| matches!(character, '\\' | '%' | '?' | '#'))
        || template.contains("//")
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Docs workspace base_path `{template}`"),
        ));
    }
    let expanded = template.replace("{name}", name);
    if expanded.contains('{') || expanded.contains('}') {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Docs workspace base_path placeholder in `{template}`"),
        ));
    }
    let segments = expanded.trim_matches('/').split('/').collect::<Vec<_>>();
    if segments.is_empty()
        || segments
            .iter()
            .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Docs workspace base_path `{template}`"),
        ));
    }
    let relative = segments.join("/");
    Ok(if site_base == "/" {
        format!("/{relative}/")
    } else {
        format!("{}{relative}/", site_base)
    })
}

pub fn build_docs(
    options: DocsBuildOptions,
    cancellation: &CancellationToken,
) -> Result<DocsBuildResult, WakeError> {
    build_docs_with_mode(options, DocsMode::Site, cancellation)
}
pub fn build_docs_with_mode(
    options: DocsBuildOptions,
    docs_mode: DocsMode,
    cancellation: &CancellationToken,
) -> Result<DocsBuildResult, WakeError> {
    if docs_mode == DocsMode::Site {
        let workspaces = discover_docs_workspaces(&options)?;
        if !workspaces.is_empty() {
            return build_aggregated_docs(options, workspaces, cancellation);
        }
    }
    build_docs_leaf(options, docs_mode, cancellation)
}

fn build_docs_leaf(
    options: DocsBuildOptions,
    docs_mode: DocsMode,
    cancellation: &CancellationToken,
) -> Result<DocsBuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let (mut prepared, docs, routes, demos, warnings) =
        prepare_docs(&options, wake_docs::BuildMode::Production, docs_mode)?;
    prepared.outdir = absolute_from(
        &prepared.root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("docs-dist")),
    );
    prepared.config.public_path = Some(normalize_public_path(&docs.base_path));
    let build_options = BuildOptions {
        project: options.project,
        entry: Some(prepared.entry.clone()),
        outdir: Some(prepared.outdir.clone()),
        write: true,
        ..BuildOptions::default()
    };
    let lifetime = if build_options.cache {
        BundlerLifetime::Session
    } else {
        BundlerLifetime::OneShot
    };
    let mut bundler = create_bundler(&prepared, &build_options, true, lifetime)?;
    bundler.set_entry_chunk_name("entry");
    let output = bundler.build(&prepared.entry);
    cancellation.check()?;
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake documentation build failed")
                .with_diagnostic_infos(diagnostic_infos(&output.diagnostics, &prepared.root)),
        );
    }
    let scripts = output
        .chunks
        .iter()
        .filter(|chunk| chunk.is_entry)
        .map(|chunk| chunk.file_name.clone())
        .collect::<Vec<_>>();
    let styles = output.entry().styles.clone();
    let html = wake_html::generate(
        None,
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: prepared.config.public_path(),
        },
    );
    let mut result = finish_output(
        &prepared,
        &build_options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
    )?;
    result
        .diagnostics
        .extend(warnings.into_iter().map(|message| DiagnosticInfo {
            severity: "warning".to_string(),
            code: Some("WAKE_DOCS".to_string()),
            message,
            path: None,
            start: None,
            end: None,
            location: None,
            notes: Vec::new(),
        }));

    wake_docs::write_route_shells(
        &prepared.outdir,
        &routes,
        &html,
        &docs.title,
        &docs.description,
        &docs.locale,
    )
    .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    wake_docs::copy_public_assets(&prepared.root, &prepared.outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DocsBuildResult {
        build: result,
        routes,
        mode: docs_mode,
        demos,
        workspaces: Vec::new(),
    })
}

fn build_aggregated_docs(
    options: DocsBuildOptions,
    workspaces: Vec<ResolvedDocsWorkspace>,
    cancellation: &CancellationToken,
) -> Result<DocsBuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    let site_root = canonical_project_root(&normalize_path(&config.resolved_root(&config_dir)))?;
    let site_base = normalize_public_path(
        options
            .base_path
            .as_deref()
            .unwrap_or(&config.docs.base_path),
    );
    let final_outdir = absolute_from(
        &site_root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("docs-dist")),
    );
    validate_output_directory(&final_outdir)?;
    let stage_parent = final_outdir.parent().unwrap_or(&site_root);
    std::fs::create_dir_all(stage_parent)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(stage_parent))?;
    let staging = tempfile::Builder::new()
        .prefix(".wake-docs-stage-")
        .tempdir_in(stage_parent)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(stage_parent))?;
    let stage_root = staging.path().join("output");

    let mut site_options = options.clone();
    site_options.outdir = Some(stage_root.clone());
    site_options.presentation = Some(DocsPresentation::Standalone);
    let mut result = build_docs_leaf(site_options, DocsMode::Site, cancellation)?;
    validate_workspace_route_mounts(&site_base, &result.routes, &workspaces)?;

    let mut workspace_infos = Vec::new();
    for workspace in workspaces {
        cancellation.check()?;
        let relative = workspace_output_relative(&site_base, &workspace.base_path)?;
        let workspace_outdir = stage_root.join(&relative);
        if workspace_outdir.exists() {
            return Err(WakeError::new(
                "WAKE_BUILD",
                format!(
                    "Docs workspace `{}` output collides with the parent site at `{}`",
                    workspace.name, workspace.base_path
                ),
            )
            .at(&workspace_outdir));
        }
        let presentation = match workspace.presentation {
            wake_config::DocsWorkspacePresentation::Embedded => DocsPresentation::Embedded,
            wake_config::DocsWorkspacePresentation::Standalone => DocsPresentation::Standalone,
        };
        let workspace_options = DocsBuildOptions {
            project: ProjectOptions {
                cwd: Some(workspace.config_dir.clone()),
                config_path: None,
            },
            outdir: Some(workspace_outdir),
            base_path: Some(workspace.base_path.clone()),
            presentation: Some(presentation),
        };
        let mut workspace_result =
            build_docs_leaf(workspace_options, DocsMode::Components, cancellation)
                .map_err(|error| scope_workspace_error(error, &workspace.name, &workspace.root))?;
        result.build.module_count += workspace_result.build.module_count;
        result.build.updated_module_count += workspace_result.build.updated_module_count;
        result.build.cached_module_count += workspace_result.build.cached_module_count;
        for file in &mut workspace_result.build.files {
            file.path = relative
                .join(&file.path)
                .to_string_lossy()
                .replace('\\', "/");
        }
        for diagnostic in &mut workspace_result.build.diagnostics {
            diagnostic
                .notes
                .push(format!("Docs workspace: {}", workspace.name));
        }
        result.build.files.extend(workspace_result.build.files);
        result
            .build
            .diagnostics
            .extend(workspace_result.build.diagnostics);
        workspace_infos.push(DocsWorkspaceBuildInfo {
            name: workspace.name,
            root: workspace.root.to_string_lossy().into_owned(),
            base_path: workspace.base_path,
            mode: DocsMode::Components,
            presentation: presentation.as_str().to_string(),
            demos: workspace_result.demos.len(),
        });
    }

    write_aggregate_docs_manifest(&stage_root, &site_base, &workspace_infos)?;
    let published_files = docs_output_inventory(&stage_root)?;
    commit_output_tree(&stage_root, &final_outdir)?;
    result.build.output_dir = Some(final_outdir.to_string_lossy().into_owned());
    result.build.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    result.build.files = published_files;
    result.workspaces = workspace_infos;
    Ok(result)
}

fn docs_output_inventory(root: &Path) -> Result<Vec<OutputFile>, WakeError> {
    collect_output_tree_files(root, "documentation")?
        .into_iter()
        .map(|relative| {
            let path = root.join(&relative);
            let bytes = std::fs::metadata(&path)
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?
                .len() as usize;
            let file_name = relative
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            let extension = relative
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or_default();
            let kind = if file_name == "manifest.json" {
                "manifest"
            } else {
                match extension {
                    "html" => "html",
                    "js" | "mjs" | "cjs" => "chunk",
                    "map" => "map",
                    _ => "asset",
                }
            };
            Ok(OutputFile {
                path: relative.to_string_lossy().replace('\\', "/"),
                kind: kind.to_string(),
                bytes,
            })
        })
        .collect()
}

fn validate_output_directory(outdir: &Path) -> Result<(), WakeError> {
    if outdir.file_name().is_none() || outdir == Path::new(".") {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "refusing to write unsafe documentation output directory: {}",
                outdir.display()
            ),
        ));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(outdir)
        && metadata.file_type().is_symlink()
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "refusing to write documentation output through a symbolic link: {}",
                outdir.display()
            ),
        )
        .at(outdir));
    }
    Ok(())
}

fn validate_workspace_route_mounts(
    site_base: &str,
    routes: &[wake_docs::RouteInfo],
    workspaces: &[ResolvedDocsWorkspace],
) -> Result<(), WakeError> {
    for workspace in workspaces {
        for route in routes {
            let relative = route.slug.trim_matches('/');
            let route_base = if relative.is_empty() {
                site_base.to_string()
            } else if site_base == "/" {
                format!("/{relative}/")
            } else {
                format!("{site_base}{relative}/")
            };
            if route_base.starts_with(&workspace.base_path) {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    format!(
                        "Docs workspace `{}` mount `{}` shadows parent site route `{}`",
                        workspace.name, workspace.base_path, route.slug
                    ),
                ));
            }
        }
    }
    Ok(())
}

fn workspace_output_relative(site_base: &str, workspace_base: &str) -> Result<PathBuf, WakeError> {
    let relative = workspace_base
        .strip_prefix(site_base)
        .ok_or_else(|| {
            WakeError::new(
                "WAKE_CONFIG",
                format!(
                    "Docs workspace base path `{workspace_base}` is outside site base `{site_base}`"
                ),
            )
        })?
        .trim_matches('/');
    if relative.is_empty() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            "Docs workspace cannot replace the parent site root",
        ));
    }
    Ok(relative.split('/').collect())
}

fn scope_workspace_error(mut error: WakeError, name: &str, root: &Path) -> WakeError {
    error.message = format!("Docs workspace `{name}` failed: {}", error.message);
    if error.path.is_none() {
        error.path = Some(root.to_string_lossy().into_owned());
    }
    for diagnostic in &mut error.diagnostics {
        diagnostic.notes.push(format!("Docs workspace: {name}"));
        if let Some(path) = &diagnostic.path {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                diagnostic.path = Some(root.join(path).to_string_lossy().into_owned());
            }
        }
    }
    error
}

fn write_aggregate_docs_manifest(
    stage_root: &Path,
    site_base: &str,
    workspaces: &[DocsWorkspaceBuildInfo],
) -> Result<(), WakeError> {
    let path = stage_root.join("manifest.json");
    let bytes = std::fs::read(&path)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    let mut manifest: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("Wake generated an invalid Docs manifest: {error}"),
        )
        .at(&path)
    })?;
    let object = manifest.as_object_mut().ok_or_else(|| {
        WakeError::new("WAKE_INTERNAL", "Wake generated a non-object Docs manifest").at(&path)
    })?;
    let values = workspaces
        .iter()
        .map(|workspace| {
            let relative = workspace_output_relative(site_base, &workspace.base_path)?;
            Ok(serde_json::json!({
                "name": workspace.name,
                "basePath": workspace.base_path,
                "manifest": relative.join("manifest.json").to_string_lossy().replace('\\', "/"),
            }))
        })
        .collect::<Result<Vec<_>, WakeError>>()?;
    object.insert("workspaces".to_string(), serde_json::Value::Array(values));
    std::fs::write(
        &path,
        serde_json::to_vec_pretty(&manifest).expect("serializable Docs manifest"),
    )
    .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
    Ok(())
}

fn commit_output_tree(staging: &Path, target: &Path) -> Result<(), WakeError> {
    commit_staged_output(staging, target, None, "documentation", ".wake-docs-backup-")
}

pub(crate) fn commit_staged_output(
    staging: &Path,
    target: &Path,
    owned_roots: Option<&[&str]>,
    product: &str,
    backup_prefix: &str,
) -> Result<(), WakeError> {
    if !staging.is_dir() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!(
                "{product} output prepare failed: staging directory does not exist: {}",
                staging.display()
            ),
        )
        .at(staging));
    }
    let staged_files = collect_scoped_output_files(staging, owned_roots, product)?;
    let existing_files = collect_scoped_output_files(target, owned_roots, product)?;
    let staged_set = staged_files.iter().cloned().collect::<BTreeSet<_>>();
    let mut replacements = Vec::new();

    for relative in staged_files {
        let source = staging.join(&relative);
        let destination = target.join(&relative);
        validate_output_file_shape(target, &relative, product)?;
        let bytes = std::fs::read(&source).map_err(|error| {
            output_commit_error(product, "prepare", "read staged file", &source, error)
        })?;
        if destination.is_file() {
            let current = std::fs::read(&destination).map_err(|error| {
                output_commit_error(
                    product,
                    "prepare",
                    "read existing output file",
                    &destination,
                    error,
                )
            })?;
            if current == bytes {
                continue;
            }
        }
        replacements.push((relative, bytes));
    }
    let stale = existing_files
        .into_iter()
        .filter(|relative| !staged_set.contains(relative))
        .collect::<Vec<_>>();
    if replacements.is_empty() && stale.is_empty() {
        return Ok(());
    }

    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent).map_err(|error| {
        output_commit_error(product, "prepare", "create output parent", parent, error)
    })?;
    let backup = tempfile::Builder::new()
        .prefix(backup_prefix)
        .tempdir_in(parent)
        .map_err(|error| {
            output_commit_error(product, "backup", "create output backup", parent, error)
        })?;
    let mut backup_paths = replacements
        .iter()
        .map(|(relative, _)| relative.clone())
        .chain(stale.iter().cloned())
        .collect::<Vec<_>>();
    backup_paths.sort();
    backup_paths.dedup();
    for relative in backup_paths {
        let current = target.join(&relative);
        if !current.is_file() {
            continue;
        }
        let saved = backup.path().join(&relative);
        let saved_parent = saved.parent().unwrap_or_else(|| backup.path());
        std::fs::create_dir_all(saved_parent).map_err(|error| {
            output_commit_error(
                product,
                "backup",
                "create output backup directory",
                saved_parent,
                error,
            )
        })?;
        std::fs::copy(&current, &saved).map_err(|error| {
            output_commit_error(product, "backup", "back up output file", &current, error)
        })?;
    }

    let mut touched = Vec::new();
    for (relative, bytes) in replacements {
        let destination = target.join(&relative);
        if let Err(error) = atomic_write(&destination, &bytes) {
            return Err(rollback_output_tree(
                target,
                backup.path(),
                &touched,
                WakeError::new(
                    "WAKE_IO",
                    format!(
                        "{product} output install failed while replacing `{}`: {}",
                        destination.display(),
                        error.message
                    ),
                )
                .at(&destination),
            ));
        }
        touched.push(relative);
    }
    for relative in stale {
        let destination = target.join(&relative);
        if let Err(error) = std::fs::remove_file(&destination) {
            return Err(rollback_output_tree(
                target,
                backup.path(),
                &touched,
                output_commit_error(
                    product,
                    "remove",
                    "remove stale output file",
                    &destination,
                    error,
                ),
            ));
        }
        touched.push(relative);
    }
    if let Err(error) = remove_empty_output_directories(target, owned_roots, product) {
        return Err(rollback_output_tree(target, backup.path(), &touched, error));
    }
    Ok(())
}

fn collect_scoped_output_files(
    base: &Path,
    owned_roots: Option<&[&str]>,
    product: &str,
) -> Result<Vec<PathBuf>, WakeError> {
    let Some(owned_roots) = owned_roots else {
        return collect_output_tree_files(base, product);
    };
    let mut files = Vec::new();
    for owned in owned_roots {
        let directory = base.join(owned);
        if !directory.exists() {
            continue;
        }
        if !directory.is_dir() {
            return Err(WakeError::new(
                "WAKE_IO",
                format!(
                    "{product} output prepare failed: expected directory `{}`",
                    directory.display()
                ),
            )
            .at(&directory));
        }
        files.extend(
            collect_output_tree_files(&directory, product)?
                .into_iter()
                .map(|relative| PathBuf::from(owned).join(relative)),
        );
    }
    files.sort();
    Ok(files)
}

fn collect_output_tree_files(base: &Path, product: &str) -> Result<Vec<PathBuf>, WakeError> {
    fn visit(
        base: &Path,
        directory: &Path,
        files: &mut Vec<PathBuf>,
        product: &str,
    ) -> Result<(), WakeError> {
        for entry in std::fs::read_dir(directory).map_err(|error| {
            output_commit_error(
                product,
                "prepare",
                "read output directory",
                directory,
                error,
            )
        })? {
            let entry = entry.map_err(|error| {
                output_commit_error(product, "prepare", "read output entry", directory, error)
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                output_commit_error(product, "prepare", "inspect output entry", &path, error)
            })?;
            if metadata.file_type().is_symlink() {
                return Err(WakeError::new(
                    "WAKE_IO",
                    format!(
                        "{product} output contains a symbolic link, which Wake will not follow: {}",
                        path.display()
                    ),
                )
                .at(&path));
            }
            if metadata.is_dir() {
                visit(base, &path, files, product)?;
            } else if metadata.is_file() {
                files.push(
                    path.strip_prefix(base)
                        .expect("visited output is below its root")
                        .to_path_buf(),
                );
            } else {
                return Err(WakeError::new(
                    "WAKE_IO",
                    format!("unsupported {product} output entry: {}", path.display()),
                )
                .at(&path));
            }
        }
        Ok(())
    }

    if !base.exists() {
        return Ok(Vec::new());
    }
    let metadata = std::fs::symlink_metadata(base).map_err(|error| {
        output_commit_error(product, "prepare", "inspect output root", base, error)
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!(
                "{product} output root is not a real directory: {}",
                base.display()
            ),
        )
        .at(base));
    }
    let mut files = Vec::new();
    visit(base, base, &mut files, product)?;
    files.sort();
    Ok(files)
}

fn validate_output_file_shape(
    target: &Path,
    relative: &Path,
    product: &str,
) -> Result<(), WakeError> {
    let destination = target.join(relative);
    if destination.is_dir() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!(
                "{product} output file collides with an existing directory: {}",
                destination.display()
            ),
        )
        .at(&destination));
    }
    let mut ancestor = relative.parent();
    while let Some(path) = ancestor {
        let destination = target.join(path);
        if destination.is_file() {
            return Err(WakeError::new(
                "WAKE_IO",
                format!(
                    "{product} output directory collides with an existing file: {}",
                    destination.display()
                ),
            )
            .at(&destination));
        }
        ancestor = path.parent();
    }
    Ok(())
}

fn remove_empty_output_directories(
    root: &Path,
    owned_roots: Option<&[&str]>,
    product: &str,
) -> Result<(), WakeError> {
    fn collect(
        directory: &Path,
        directories: &mut Vec<PathBuf>,
        product: &str,
    ) -> Result<bool, WakeError> {
        if !directory.is_dir() {
            return Ok(false);
        }
        let mut contains_files = false;
        for entry in std::fs::read_dir(directory).map_err(|error| {
            output_commit_error(
                product,
                "cleanup",
                "read output directory",
                directory,
                error,
            )
        })? {
            let path = entry
                .map_err(|error| {
                    output_commit_error(product, "cleanup", "read output entry", directory, error)
                })?
                .path();
            if path.is_dir() {
                contains_files |= collect(&path, directories, product)?;
            } else {
                contains_files = true;
            }
        }
        if !contains_files {
            directories.push(directory.to_path_buf());
        }
        Ok(contains_files)
    }

    if !root.is_dir() {
        return Ok(());
    }
    let mut directories = Vec::new();
    if let Some(owned_roots) = owned_roots {
        for owned in owned_roots {
            collect(&root.join(owned), &mut directories, product)?;
        }
    } else {
        collect(root, &mut directories, product)?;
        directories.retain(|directory| directory != root);
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        match std::fs::remove_dir(&directory) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                ) => {}
            Err(error) => {
                return Err(output_commit_error(
                    product,
                    "cleanup",
                    "remove empty output directory",
                    &directory,
                    error,
                ));
            }
        }
    }
    Ok(())
}

fn rollback_output_tree(
    target: &Path,
    backup: &Path,
    touched: &[PathBuf],
    primary: WakeError,
) -> WakeError {
    let mut failures = Vec::new();
    for relative in touched.iter().rev() {
        let saved = backup.join(relative);
        let destination = target.join(relative);
        let result = if saved.is_file() {
            std::fs::read(&saved)
                .map_err(|error| error.to_string())
                .and_then(|bytes| atomic_write(&destination, &bytes).map_err(|error| error.message))
        } else if destination.exists() {
            std::fs::remove_file(&destination).map_err(|error| error.to_string())
        } else {
            Ok(())
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error}", destination.display()));
        }
    }
    if failures.is_empty() {
        primary
    } else {
        WakeError::new(
            primary.code,
            format!(
                "{}; rollback also failed for {}",
                primary.message,
                failures.join(", ")
            ),
        )
        .at(Path::new(primary.path.as_deref().unwrap_or(".")))
    }
}

fn output_commit_error(
    product: &str,
    phase: &str,
    operation: &str,
    path: &Path,
    error: std::io::Error,
) -> WakeError {
    WakeError::new(
        "WAKE_IO",
        format!(
            "{product} output {phase} failed while attempting to {operation} `{}`: {error}",
            path.display()
        ),
    )
    .at(path)
}

pub fn start_docs_dev_server(options: DevServerOptions) -> Result<DevServer, WakeError> {
    start_docs_dev_server_with_mode(options, DocsMode::Site)
}
pub fn start_docs_dev_server_with_mode(
    options: DevServerOptions,
    docs_mode: DocsMode,
) -> Result<DevServer, WakeError> {
    if docs_mode == DocsMode::Site {
        let docs_options = DocsBuildOptions {
            project: options.project.clone(),
            outdir: None,
            base_path: None,
            presentation: None,
        };
        let workspaces = discover_docs_workspaces(&docs_options)?;
        if !workspaces.is_empty() {
            return start_aggregated_docs_dev_server(options, docs_options, workspaces);
        }
    }
    start_docs_dev_server_leaf(options, docs_mode)
}

fn start_docs_dev_server_leaf(
    options: DevServerOptions,
    docs_mode: DocsMode,
) -> Result<DevServer, WakeError> {
    let docs_options = DocsBuildOptions {
        project: options.project.clone(),
        outdir: None,
        base_path: None,
        presentation: None,
    };
    let (prepared, docs, _routes, _demos, warnings) =
        prepare_docs(&docs_options, wake_docs::BuildMode::Development, docs_mode)?;
    let config = &prepared.config;
    let port = options.port.or(config.dev_server.port).unwrap_or(5173);
    let host = options
        .host
        .or_else(|| config.dev_server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let (event_tx, event_rx) = mpsc::channel();
    for warning in warnings {
        let diagnostic = Diagnostic::warning(warning).with_code("WAKE_DOCS");
        let _ = event_tx.send(DevServerEvent::Diagnostic {
            diagnostic: DiagnosticInfo::from_diagnostic(&diagnostic, None),
        });
    }
    let event_root = prepared.root.clone();
    let forwarded_tx = event_tx.clone();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        forward_dev_server_event(&forwarded_tx, &event_root, event);
    });
    let docs_base_path = docs.base_path.clone();
    let before_rebuild = docs_before_rebuild(
        prepared.root.clone(),
        config.clone(),
        docs,
        docs_mode,
        event_tx.clone(),
        None,
        None,
        false,
    );
    let serve_options = wake_dev_server::ServeOptions {
        entry: prepared.entry,
        base_path: docs_base_path,
        resolve_options: ResolveOptions {
            alias: prepared.aliases,
            conditions: ["browser", "development", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..ResolveOptions::default()
        },
        define: build_defines(config, true),
        host,
        open: options.open.unwrap_or(config.dev_server.open),
        proxy: config
            .dev_server
            .proxy
            .iter()
            .map(|proxy| wake_dev_server::ProxyRule {
                context: proxy.context.clone(),
                target: proxy.target.clone(),
                path_rewrite: proxy
                    .path_rewrite
                    .iter()
                    .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                    .collect(),
                change_origin: proxy.change_origin,
            })
            .collect(),
        target_env: resolve_target_env(config, &prepared.root)?,
        jsx_import_source: config.react.jsx_import_source.clone(),
        watch_roots: docs_watch_roots(&prepared.root, config),
        before_rebuild: Some(before_rebuild),
        quiet: true,
        event_handler: Some(event_handler),
        mounts: Vec::new(),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
    })
}

fn start_aggregated_docs_dev_server(
    options: DevServerOptions,
    docs_options: DocsBuildOptions,
    workspaces: Vec<ResolvedDocsWorkspace>,
) -> Result<DevServer, WakeError> {
    let (prepared, site_docs, _routes, _demos, warnings) = prepare_docs(
        &docs_options,
        wake_docs::BuildMode::Development,
        DocsMode::Site,
    )?;
    let config = prepared.config.clone();
    let port = options.port.or(config.dev_server.port).unwrap_or(5173);
    let host = options
        .host
        .or_else(|| config.dev_server.host.clone())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let (event_tx, event_rx) = mpsc::channel();
    send_docs_warnings(&event_tx, warnings, None);
    let event_root = prepared.root.clone();
    let forwarded_tx = event_tx.clone();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        forward_dev_server_event(&forwarded_tx, &event_root, event);
    });

    let topology = docs_workspace_topology(&workspaces);
    let topology_options = docs_options.clone();
    let site_before_rebuild = docs_before_rebuild(
        prepared.root.clone(),
        config.clone(),
        site_docs.clone(),
        DocsMode::Site,
        event_tx.clone(),
        None,
        Some(Arc::new(move |changed| {
            if !changed.iter().any(|path| {
                path.file_name()
                    .is_some_and(|name| name == wake_config::CONFIG_FILE)
            }) {
                return Ok(());
            }
            let discovered =
                discover_docs_workspaces(&topology_options).map_err(|error| error.to_string())?;
            if docs_workspace_topology(&discovered) != topology {
                return Err(
                    "Docs workspace topology changed; restart the development server".to_string(),
                );
            }
            Ok(())
        })),
        true,
    );

    let mut mounts = Vec::new();
    for workspace in workspaces {
        let presentation = match workspace.presentation {
            wake_config::DocsWorkspacePresentation::Embedded => DocsPresentation::Embedded,
            wake_config::DocsWorkspacePresentation::Standalone => DocsPresentation::Standalone,
        };
        let workspace_options = DocsBuildOptions {
            project: ProjectOptions {
                cwd: Some(workspace.config_dir.clone()),
                config_path: None,
            },
            outdir: None,
            base_path: Some(workspace.base_path.clone()),
            presentation: Some(presentation),
        };
        let (workspace_prepared, workspace_docs, _routes, _demos, warnings) = prepare_docs(
            &workspace_options,
            wake_docs::BuildMode::Development,
            DocsMode::Components,
        )
        .map_err(|error| scope_workspace_error(error, &workspace.name, &workspace.root))?;
        send_docs_warnings(&event_tx, warnings, Some(&workspace.name));
        let workspace_config = workspace_prepared.config.clone();
        let before_rebuild = docs_before_rebuild(
            workspace_prepared.root.clone(),
            workspace_config.clone(),
            workspace_docs,
            DocsMode::Components,
            event_tx.clone(),
            Some(workspace.name.clone()),
            None,
            true,
        );
        let resolve_options = ResolveOptions {
            alias: workspace_prepared.aliases,
            conditions: ["browser", "development", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..ResolveOptions::default()
        };
        let target_env = resolve_target_env(&workspace_config, &workspace_prepared.root)?;
        mounts.push(wake_dev_server::MountedServeOptions {
            name: workspace.name,
            root: workspace_prepared.root.clone(),
            base_path: workspace.base_path,
            loading: match workspace.dev_loading {
                wake_config::DocsWorkspaceDevLoading::Lazy => wake_dev_server::DevLoading::Lazy,
                wake_config::DocsWorkspaceDevLoading::Eager => wake_dev_server::DevLoading::Eager,
            },
            entry: workspace_prepared.entry,
            resolve_options,
            define: build_defines(&workspace_config, true),
            target_env,
            jsx_import_source: workspace_config.react.jsx_import_source.clone(),
            watch_roots: docs_watch_roots(&workspace_prepared.root, &workspace_config),
            before_rebuild: Some(before_rebuild),
        });
    }

    let serve_options = wake_dev_server::ServeOptions {
        entry: prepared.entry,
        base_path: site_docs.base_path,
        resolve_options: ResolveOptions {
            alias: prepared.aliases,
            conditions: ["browser", "development", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..ResolveOptions::default()
        },
        define: build_defines(&config, true),
        host,
        open: options.open.unwrap_or(config.dev_server.open),
        proxy: config
            .dev_server
            .proxy
            .iter()
            .map(|proxy| wake_dev_server::ProxyRule {
                context: proxy.context.clone(),
                target: proxy.target.clone(),
                path_rewrite: proxy
                    .path_rewrite
                    .iter()
                    .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                    .collect(),
                change_origin: proxy.change_origin,
            })
            .collect(),
        target_env: resolve_target_env(&config, &prepared.root)?,
        jsx_import_source: config.react.jsx_import_source.clone(),
        watch_roots: docs_watch_roots(&prepared.root, &config),
        before_rebuild: Some(site_before_rebuild),
        quiet: true,
        event_handler: Some(event_handler),
        mounts,
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
    })
}

type DocsTopologyCheck = Arc<dyn Fn(&[PathBuf]) -> Result<(), String> + Send + Sync + 'static>;

fn docs_before_rebuild(
    root: PathBuf,
    config: wake_config::Config,
    docs: wake_docs::DocsOptions,
    docs_mode: DocsMode,
    event_tx: mpsc::Sender<DevServerEvent>,
    workspace: Option<String>,
    topology_check: Option<DocsTopologyCheck>,
    lock_base_path: bool,
) -> wake_dev_server::BeforeRebuild {
    let state = Arc::new(Mutex::new((config, docs)));
    Arc::new(move |changed| {
        if let Some(check) = &topology_check {
            check(changed)?;
        }
        if let Some(config_path) = changed.iter().find(|path| {
            path.file_name()
                .is_some_and(|name| name == wake_config::CONFIG_FILE)
        }) {
            let config_dir = config_path.parent().unwrap_or(&root);
            let refreshed_config =
                wake_config::load(config_dir).map_err(|error| error.to_string())?;
            let refreshed_root = canonical_project_root(&normalize_path(
                &refreshed_config.resolved_root(config_dir),
            ))
            .map_err(|error| error.to_string())?;
            if refreshed_root != root {
                return Err("Docs project root changed; restart the development server".to_string());
            }
            let mut state = state.lock().unwrap();
            let previous_docs = &state.1;
            if refreshed_config.docs.source_dir != state.0.docs.source_dir
                || refreshed_config.docs.preview != state.0.docs.preview
                || refreshed_config.docs.theme_css != state.0.docs.theme_css
            {
                return Err(
                    "Docs source, Preview, or theme file topology changed; restart the development server"
                        .to_string(),
                );
            }
            let mut refreshed_docs =
                docs_options(&refreshed_config, None, previous_docs.presentation);
            if lock_base_path {
                refreshed_docs.base_path = previous_docs.base_path.clone();
            }
            *state = (refreshed_config, refreshed_docs);
        }
        let (config, docs) = state.lock().unwrap().clone();
        let generated = wake_docs::generate_with_mode(
            &root,
            &docs,
            wake_docs::BuildMode::Development,
            docs_mode,
        )
        .map_err(|error| error.to_string())?;
        send_docs_warnings(&event_tx, generated.warnings, workspace.as_deref());
        let mut invalidated = generated.changed_files;
        invalidated.extend(
            prepare_aliases_and_scans(&config, &root)
                .map_err(|error| error.to_string())?
                .into_iter()
                .filter_map(|(name, path)| name.starts_with("@@@/").then_some(path)),
        );
        Ok(invalidated)
    })
}

fn docs_watch_roots(root: &Path, config: &wake_config::Config) -> Vec<PathBuf> {
    let mut roots = vec![
        root.join(&config.docs.source_dir),
        root.join("src"),
        root.join(wake_config::CONFIG_FILE),
        root.join("navigation.toml"),
    ];
    if let Some(preview) = &config.docs.preview {
        roots.push(root.join(preview));
    }
    if let Some(theme_css) = &config.docs.theme_css {
        roots.push(root.join(theme_css));
    }
    roots.extend(
        config
            .component_scan
            .iter()
            .map(|rule| root.join(&rule.cwd)),
    );
    roots.sort();
    roots.dedup();
    roots
}

fn send_docs_warnings(
    sender: &mpsc::Sender<DevServerEvent>,
    warnings: Vec<String>,
    workspace: Option<&str>,
) {
    for warning in warnings {
        let message = workspace
            .map(|workspace| format!("Docs workspace `{workspace}`: {warning}"))
            .unwrap_or(warning);
        let diagnostic = Diagnostic::warning(message).with_code("WAKE_DOCS");
        let _ = sender.send(DevServerEvent::Diagnostic {
            diagnostic: DiagnosticInfo::from_diagnostic(&diagnostic, None),
        });
    }
}

fn docs_workspace_topology(
    workspaces: &[ResolvedDocsWorkspace],
) -> Vec<(String, String, String, &'static str, &'static str)> {
    workspaces
        .iter()
        .map(|workspace| {
            (
                workspace.name.clone(),
                workspace.root.to_string_lossy().into_owned(),
                workspace.base_path.clone(),
                workspace.presentation.as_str(),
                workspace.dev_loading.as_str(),
            )
        })
        .collect()
}

fn prepare_docs(
    options: &DocsBuildOptions,
    mode: wake_docs::BuildMode,
    docs_mode: DocsMode,
) -> Result<PreparedDocs, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_dir))?;
    let configured_root = normalize_path(&config.resolved_root(&config_dir));
    if !configured_root.is_dir() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "configured project root does not exist: {}",
                configured_root.display()
            ),
        )
        .at(&configured_root));
    }
    let root = canonical_project_root(&configured_root)?;
    let mut aliases = prepare_aliases_and_scans(&config, &root)?;
    let docs = docs_options(
        &config,
        options.base_path.as_deref(),
        options.presentation.unwrap_or_default(),
    );
    let generated = wake_docs::generate_with_mode(&root, &docs, mode, docs_mode)
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    aliases.retain(|(name, _)| name != "@@wake/docs" && name != "@@wake/docs-project");
    aliases.extend(generated.aliases);
    let routes = generated.routes;
    let demos = generated.demos;
    let warnings = generated.warnings;
    Ok((
        PreparedBuild {
            root: root.clone(),
            entry: generated.entry,
            outdir: root.join("docs-dist"),
            config,
            aliases,
        },
        docs,
        routes,
        demos,
        warnings,
    ))
}

fn docs_options(
    config: &wake_config::Config,
    base_path: Option<&str>,
    presentation: DocsPresentation,
) -> wake_docs::DocsOptions {
    let docs = &config.docs;
    wake_docs::DocsOptions {
        source_dir: PathBuf::from(&docs.source_dir),
        title: docs.title.clone(),
        description: docs.description.clone(),
        locale: docs.locale.clone(),
        logo: docs.logo.clone(),
        repository_url: docs.repository_url.clone(),
        base_path: base_path.unwrap_or(&docs.base_path).to_string(),
        preview: docs.preview.as_deref().map(PathBuf::from),
        theme_css: docs.theme_css.as_deref().map(PathBuf::from),
        default_theme: docs.default_theme.clone(),
        accent_color: docs.accent_color.clone(),
        presentation,
    }
}

fn normalize_public_path(path: &str) -> String {
    if path.trim().is_empty() || path == "/" {
        "/".to_string()
    } else {
        format!("/{}/", path.trim_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    fn assert_same_existing_file(actual: impl AsRef<Path>, expected: impl AsRef<Path>) {
        let actual = std::fs::canonicalize(actual.as_ref()).unwrap();
        let expected = std::fs::canonicalize(expected.as_ref()).unwrap();
        assert_eq!(
            wake_common::fs::normalize(&actual),
            wake_common::fs::normalize(&expected)
        );
    }

    #[test]
    fn test_session_event_fields_use_the_public_camel_case_contract() {
        let event = TestSessionEvent::TestCaseResult {
            run_id: "run-public-contract".to_string(),
            suite_id: "suite-public-contract".to_string(),
            result: Box::new(TestCaseResult {
                id: "case-public-contract".to_string(),
                name: "case".to_string(),
                full_name: "suite case".to_string(),
                status: TestStatus::Passed,
                duration_ms: 1,
                assertions: 1,
                attempts: 1,
                location: None,
                failures: Vec::new(),
            }),
        };
        let serialized = serde_json::to_value(event).unwrap();

        assert_eq!(serialized["type"], "testCaseResult");
        assert_eq!(serialized["runId"], "run-public-contract");
        assert_eq!(serialized["suiteId"], "suite-public-contract");
        assert!(serialized.get("run_id").is_none());
        assert!(serialized.get("suite_id").is_none());
    }

    fn test_protocol_pair() -> (TestProtocolSession, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        (
            TestProtocolSession::new(client, "test-token".to_string()).unwrap(),
            server,
        )
    }

    fn test_host_response(request_id: u64, sequence: u64, body: HostResponseBody) -> HostResponse {
        HostResponse {
            protocol_version: PROTOCOL_VERSION,
            build_id: HOST_BUILD_ID.to_string(),
            request_id,
            sequence,
            body,
        }
    }

    fn empty_test_result(run_id: String) -> TestRunResult {
        TestRunResult::empty(
            run_id,
            "test-seed".to_string(),
            wake_test_contract::TestEnvironmentInfo {
                kind: "dom".to_string(),
                react: None,
                react_dom: None,
                v8: "test-v8".to_string(),
                browser: None,
            },
        )
    }

    #[test]
    fn test_protocol_reuses_one_connection_for_run_events_and_shutdown() {
        let (mut protocol, mut server) = test_protocol_pair();
        let host = thread::spawn(move || {
            let run: HostRequest = wake_test_contract::protocol::read_frame(&mut server).unwrap();
            let HostCommand::Run { run_id, .. } = run.command else {
                panic!("expected run command");
            };
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    1,
                    HostResponseBody::Ack {
                        command: HostAck::Run {
                            run_id: run_id.clone(),
                        },
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    2,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::RunStart {
                            run_id: run_id.clone(),
                            watching: false,
                        }),
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    3,
                    HostResponseBody::Result {
                        run_id: run_id.clone(),
                        result: Box::new(empty_test_result(run_id)),
                    },
                ),
            )
            .unwrap();

            let shutdown: HostRequest =
                wake_test_contract::protocol::read_frame(&mut server).unwrap();
            assert!(matches!(shutdown.command, HostCommand::Shutdown));
            write_frame(
                &mut server,
                &test_host_response(
                    shutdown.request_id,
                    4,
                    HostResponseBody::Ack {
                        command: HostAck::Shutdown,
                    },
                ),
            )
            .unwrap();
        });

        let result = protocol
            .run(TestOptions::default(), &CancellationToken::default())
            .unwrap();
        assert!(result.success);
        let events = protocol.drain_events();
        assert!(matches!(
            &events[..],
            [
                TestSessionEvent::RunStart { run_id, watching: false },
                TestSessionEvent::RunComplete { result }
            ] if run_id == &result.run_id
        ));
        protocol.shutdown().unwrap();
        protocol.close_transport();
        host.join().unwrap();
    }

    #[test]
    fn test_protocol_cancel_uses_the_active_connection_and_resolves_cancelled_result() {
        let (mut protocol, mut server) = test_protocol_pair();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let host = thread::spawn(move || {
            let run: HostRequest = wake_test_contract::protocol::read_frame(&mut server).unwrap();
            let HostCommand::Run { run_id, .. } = run.command else {
                panic!("expected run command");
            };
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    1,
                    HostResponseBody::Ack {
                        command: HostAck::Run {
                            run_id: run_id.clone(),
                        },
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    2,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::RunStart {
                            run_id: run_id.clone(),
                            watching: false,
                        }),
                    },
                ),
            )
            .unwrap();
            started_tx.send(()).unwrap();

            let cancel: HostRequest =
                wake_test_contract::protocol::read_frame(&mut server).unwrap();
            assert!(matches!(
                &cancel.command,
                HostCommand::Cancel { run_id: cancelled } if cancelled == &run_id
            ));
            write_frame(
                &mut server,
                &test_host_response(
                    cancel.request_id,
                    3,
                    HostResponseBody::Ack {
                        command: HostAck::Cancel {
                            run_id: run_id.clone(),
                        },
                    },
                ),
            )
            .unwrap();
            let mut result = empty_test_result(run_id.clone());
            result.success = false;
            result.termination_reason = TestTerminationReason::Cancelled;
            write_frame(
                &mut server,
                &test_host_response(
                    run.request_id,
                    4,
                    HostResponseBody::Result {
                        run_id,
                        result: Box::new(result),
                    },
                ),
            )
            .unwrap();

            let shutdown: HostRequest =
                wake_test_contract::protocol::read_frame(&mut server).unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    shutdown.request_id,
                    5,
                    HostResponseBody::Ack {
                        command: HostAck::Shutdown,
                    },
                ),
            )
            .unwrap();
        });
        let cancellation = CancellationToken::default();
        let cancelling = cancellation.clone();
        let canceller = thread::spawn(move || {
            started_rx.recv().unwrap();
            cancelling.cancel();
        });

        let result = protocol.run(TestOptions::default(), &cancellation).unwrap();
        assert!(!result.success);
        assert_eq!(result.termination_reason, TestTerminationReason::Cancelled);
        protocol.shutdown().unwrap();
        protocol.close_transport();
        canceller.join().unwrap();
        host.join().unwrap();
    }

    #[test]
    fn test_protocol_validates_build_request_and_sequence_envelopes() {
        for invalid in ["build", "request", "sequence"] {
            let (mut protocol, mut server) = test_protocol_pair();
            let host = thread::spawn(move || {
                let request: HostRequest =
                    wake_test_contract::protocol::read_frame(&mut server).unwrap();
                let HostCommand::StartWatch { watch_id, .. } = request.command else {
                    panic!("expected start-watch command");
                };
                let mut response = test_host_response(
                    request.request_id,
                    1,
                    HostResponseBody::Ack {
                        command: HostAck::StartWatch { watch_id },
                    },
                );
                match invalid {
                    "build" => response.build_id = "incompatible-build".to_string(),
                    "request" => response.request_id += 1,
                    "sequence" => response.sequence += 1,
                    _ => unreachable!(),
                }
                write_frame(&mut server, &response).unwrap();
            });

            let error = protocol.start_watch(TestOptions::default()).unwrap_err();
            assert_eq!(error.code, "WAKE_TEST_HOST", "{invalid}");
            protocol.close_transport();
            host.join().unwrap();
        }
    }

    #[test]
    fn test_protocol_pre_start_watch_error_is_diagnostic_and_the_next_run_recovers() {
        let (mut protocol, mut server) = test_protocol_pair();
        let host = thread::spawn(move || {
            let start: HostRequest = wake_test_contract::protocol::read_frame(&mut server).unwrap();
            let HostCommand::StartWatch { watch_id, .. } = start.command else {
                panic!("expected start-watch command");
            };
            write_frame(
                &mut server,
                &test_host_response(
                    start.request_id,
                    1,
                    HostResponseBody::Ack {
                        command: HostAck::StartWatch {
                            watch_id: watch_id.clone(),
                        },
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    start.request_id,
                    2,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::WatchReady {
                            watch_id: watch_id.clone(),
                            root: "/workspace".to_string(),
                        }),
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    start.request_id,
                    3,
                    HostResponseBody::WatchRunError {
                        watch_id: watch_id.clone(),
                        run_id: None,
                        started: false,
                        error: HostError {
                            code: "WAKE_TEST_RUNTIME".to_string(),
                            message: "temporary pre-start failure".to_string(),
                            path: Some("view.test.tsx".to_string()),
                        },
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    start.request_id,
                    4,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::RunStart {
                            run_id: "watch-run-recovery".to_string(),
                            watching: true,
                        }),
                    },
                ),
            )
            .unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    start.request_id,
                    5,
                    HostResponseBody::Event {
                        event: Box::new(HostEvent::RunComplete {
                            watch_id: watch_id.clone(),
                            run_id: "watch-run-recovery".to_string(),
                            result: Box::new(empty_test_result("watch-run-recovery".to_string())),
                        }),
                    },
                ),
            )
            .unwrap();

            let stop: HostRequest = wake_test_contract::protocol::read_frame(&mut server).unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    stop.request_id,
                    6,
                    HostResponseBody::Ack {
                        command: HostAck::StopWatch { watch_id },
                    },
                ),
            )
            .unwrap();
            let shutdown: HostRequest =
                wake_test_contract::protocol::read_frame(&mut server).unwrap();
            write_frame(
                &mut server,
                &test_host_response(
                    shutdown.request_id,
                    7,
                    HostResponseBody::Ack {
                        command: HostAck::Shutdown,
                    },
                ),
            )
            .unwrap();
        });

        protocol.start_watch(TestOptions::default()).unwrap();
        for _ in 0..100 {
            protocol.poll_watch_events().unwrap();
            if protocol
                .events
                .iter()
                .any(|event| matches!(event, TestSessionEvent::RunComplete { .. }))
            {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        let events = protocol.drain_events();
        assert!(matches!(
            &events[..],
            [
                TestSessionEvent::Diagnostic { run_id: None, diagnostic },
                TestSessionEvent::RunStart { run_id: recovered, .. },
                TestSessionEvent::RunComplete { result },
            ] if diagnostic.code == "WAKE_TEST_RUNTIME"
                && recovered == "watch-run-recovery"
                && result.run_id == *recovered
        ));
        protocol.stop_watch().unwrap();
        protocol.shutdown().unwrap();
        protocol.close_transport();
        host.join().unwrap();
    }

    #[test]
    fn test_protocol_started_watch_error_is_diagnostic_then_terminal() {
        let (mut protocol, mut server) = test_protocol_pair();
        let host = thread::spawn(move || {
            let start: HostRequest = wake_test_contract::protocol::read_frame(&mut server).unwrap();
            let HostCommand::StartWatch { watch_id, .. } = start.command else {
                panic!("expected start-watch command");
            };
            for response in [
                HostResponseBody::Ack {
                    command: HostAck::StartWatch {
                        watch_id: watch_id.clone(),
                    },
                },
                HostResponseBody::Event {
                    event: Box::new(HostEvent::WatchReady {
                        watch_id: watch_id.clone(),
                        root: "/workspace".to_string(),
                    }),
                },
                HostResponseBody::Event {
                    event: Box::new(HostEvent::RunStart {
                        run_id: "fatal-run".to_string(),
                        watching: true,
                    }),
                },
                HostResponseBody::WatchRunError {
                    watch_id,
                    run_id: Some("fatal-run".to_string()),
                    started: true,
                    error: HostError {
                        code: "WAKE_TEST_HOST".to_string(),
                        message: "fatal host failure".to_string(),
                        path: None,
                    },
                },
            ]
            .into_iter()
            .enumerate()
            {
                write_frame(
                    &mut server,
                    &test_host_response(start.request_id, response.0 as u64 + 1, response.1),
                )
                .unwrap();
            }
        });

        protocol.start_watch(TestOptions::default()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let error = loop {
            match protocol.poll_watch_events() {
                Ok(()) if Instant::now() < deadline => thread::sleep(Duration::from_millis(1)),
                Ok(()) => panic!("watch terminal did not arrive before the deadline"),
                Err(error) => break error,
            }
        };
        assert_eq!(error.code, "WAKE_TEST_HOST");
        assert!(!protocol.is_watching());
        assert!(matches!(
            &protocol.drain_events()[..],
            [
                TestSessionEvent::RunStart { run_id, .. },
                TestSessionEvent::Diagnostic {
                    run_id: Some(failed),
                    diagnostic,
                },
                TestSessionEvent::Closed,
            ] if run_id == "fatal-run"
                && failed == run_id
                && diagnostic.message == "fatal host failure"
        ));
        protocol.close_transport();
        host.join().unwrap();
    }

    #[test]
    fn test_protocol_started_run_error_closes_the_public_event_sequence() {
        let (mut protocol, mut server) = test_protocol_pair();
        let host = thread::spawn(move || {
            let request: HostRequest =
                wake_test_contract::protocol::read_frame(&mut server).unwrap();
            let HostCommand::Run { run_id, .. } = request.command else {
                panic!("expected run command");
            };
            for (index, body) in [
                HostResponseBody::Ack {
                    command: HostAck::Run {
                        run_id: run_id.clone(),
                    },
                },
                HostResponseBody::Event {
                    event: Box::new(HostEvent::RunStart {
                        run_id: run_id.clone(),
                        watching: false,
                    }),
                },
                HostResponseBody::Error {
                    run_id: Some(run_id),
                    error: HostError {
                        code: "WAKE_TEST_CONFIG".to_string(),
                        message: "invalid test configuration".to_string(),
                        path: None,
                    },
                },
            ]
            .into_iter()
            .enumerate()
            {
                write_frame(
                    &mut server,
                    &test_host_response(request.request_id, index as u64 + 1, body),
                )
                .unwrap();
            }
        });

        let error = protocol
            .run(TestOptions::default(), &CancellationToken::default())
            .unwrap_err();
        assert_eq!(error.code, "WAKE_TEST_CONFIG");
        assert!(matches!(
            &protocol.drain_events()[..],
            [
                TestSessionEvent::RunStart {
                    watching: false,
                    ..
                },
                TestSessionEvent::Closed,
            ]
        ));
        host.join().unwrap();
    }

    #[test]
    fn test_host_state_errors_preserve_their_public_codes() {
        for code in [
            "WAKE_TEST_BUSY",
            "WAKE_TEST_UNKNOWN_RUN",
            "WAKE_TEST_UNKNOWN_WATCH",
        ] {
            let error = wake_error_from_host(HostError {
                code: code.to_string(),
                message: "state error".to_string(),
                path: None,
            });
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn diagnostic_locations_preserve_unicode_crlf_and_safe_fallbacks() {
        let text = "const first = 1;\r\n\tconst 名 = ;\r\n";
        let start = text.find('名').unwrap() as u32;
        let end = start + '名'.len_utf8() as u32;
        let source = SourceFile::new("src/index.ts", text);
        let diagnostic = Diagnostic::error("Unexpected token")
            .with_code("WAKE_PARSE")
            .with_path("src/index.ts")
            .with_primary(wake_common::Span::new(start, end), "expected expression");
        let info = DiagnosticInfo::from_diagnostic(&diagnostic, Some(&source));
        let location = info.location.expect("valid source location");
        assert_eq!(location.line, 2);
        assert_eq!(location.column, 8);
        assert_eq!(location.end_line, 2);
        assert_eq!(location.end_column, 9);
        assert_eq!(location.line_text, "\tconst 名 = ;");
        assert_eq!(location.label.as_deref(), Some("expected expression"));

        let invalid = Diagnostic::error("invalid")
            .with_path("src/index.ts")
            .with_primary(wake_common::Span::new(0, source.len() + 1), "outside");
        assert!(
            DiagnosticInfo::from_diagnostic(&invalid, Some(&source))
                .location
                .is_none()
        );
        let missing = diagnostic_infos(
            &[Diagnostic::error("missing")
                .with_path("does-not-exist.ts")
                .with_primary(wake_common::Span::new(0, 1), "missing")],
            Path::new("."),
        );
        assert!(missing[0].location.is_none());
    }

    #[test]
    fn build_defines_disable_esm_hmr_syntax_in_classic_script_chunks() {
        let config = wake_config::Config::default();
        let defines = build_defines(&config, true);

        assert!(
            defines
                .iter()
                .any(|(key, value)| key == "import.meta.hot" && value == "false")
        );
        assert!(
            defines
                .iter()
                .any(|(key, value)| key == "import.meta.url"
                    && value == "__wake_require__.metaUrl()")
        );
    }

    #[test]
    fn docs_production_chunks_own_their_extracted_styles() {
        let fs = wake_common::MemoryFileSystem::from_files([
            (
                "src/index.js",
                "export const lazy = () => import('./route.js');",
            ),
            (
                "src/route.js",
                "import './route.css'; export const page = 'route';",
            ),
            ("src/route.css", ".route { color: red; }"),
        ]);
        let mut bundler = IncrementalBundler::new(Arc::new(fs));
        bundler.enable_code_splitting().enable_css_extraction();

        let output = bundler.build(Path::new("src/index.js"));
        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert!(
            output.chunks.len() > 1,
            "Docs production must retain route splitting"
        );
        assert!(output.entry().styles.is_empty());
        let route = output
            .chunks
            .iter()
            .find(|chunk| !chunk.is_entry && chunk.name == "route")
            .expect("route chunk");
        assert_eq!(route.styles.len(), 1);
        assert!(route.styles[0].ends_with(".css"));
        assert!(output.bundle.contains("__wake__.s"));
    }

    struct Fixture(PathBuf);

    impl Fixture {
        fn new(label: &str) -> Self {
            let sequence = NEXT_FIXTURE.fetch_add(1, AtomicOrdering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "wake-app-{label}-{}-{sequence}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(
                root.join(wake_config::CONFIG_FILE),
                "[html]\nentry = \"src/index.js\"\n",
            )
            .unwrap();
            std::fs::write(root.join("src/index.js"), "export const value = 42;\n").unwrap();
            Self(root)
        }

        fn project(&self) -> ProjectOptions {
            ProjectOptions {
                cwd: Some(self.0.clone()),
                config_path: None,
            }
        }

        fn write(&self, path: &str, contents: impl AsRef<[u8]>) {
            let path = self.0.join(path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn docs_workspace_discovery_is_direct_stable_and_base_prefixed() {
        let fixture = Fixture::new("docs-workspaces");
        fixture.write(
            wake_config::CONFIG_FILE,
            r#"[docs]
base_path = "/docs/"

[[docs.workspace]]
root = "components"
include = ["rc-*"]
base_path = "/components/{name}/workbench/"
"#,
        );
        for name in ["rc-zeta", "ignored", "rc-alpha"] {
            fixture.write(&format!("components/{name}/wake.config.toml"), "");
        }
        fixture.write("components/rc-nested/child/wake.config.toml", "");

        let workspaces = discover_docs_workspaces(&DocsBuildOptions {
            project: fixture.project(),
            ..DocsBuildOptions::default()
        })
        .unwrap();
        assert_eq!(
            workspaces
                .iter()
                .map(|workspace| workspace.name.as_str())
                .collect::<Vec<_>>(),
            ["rc-alpha", "rc-zeta"]
        );
        assert_eq!(
            workspaces[0].base_path,
            "/docs/components/rc-alpha/workbench/"
        );
        assert_eq!(
            workspaces[0].presentation,
            wake_config::DocsWorkspacePresentation::Embedded
        );
        assert_eq!(
            workspaces[0].dev_loading,
            wake_config::DocsWorkspaceDevLoading::Lazy
        );
    }

    #[test]
    fn docs_workspace_rules_reject_ambiguous_paths_and_patterns() {
        assert!(wildcard_name_match("rc-grid", "rc-*"));
        assert!(wildcard_name_match("rc-a", "rc-?"));
        assert!(!wildcard_name_match("RC-grid", "rc-*"));
        assert!(
            effective_workspace_base("/docs/", "/components/{other}/", "rc-grid")
                .unwrap_err()
                .message
                .contains("placeholder")
        );
        assert!(effective_workspace_base("/../", "/components/{name}/", "rc-grid").is_err());
        assert!(effective_workspace_base("/", "/components/../{name}/", "rc-grid").is_err());
    }

    #[test]
    fn docs_output_commit_skips_equal_files_and_removes_stale_files() {
        let fixture = Fixture::new("docs-commit");
        let staging = fixture.0.join("stage");
        let target = fixture.0.join("docs-dist");
        std::fs::create_dir_all(staging.join("assets")).unwrap();
        std::fs::create_dir_all(target.join("assets")).unwrap();
        std::fs::write(staging.join("index.html"), "same").unwrap();
        std::fs::write(staging.join("assets/new.js"), "new").unwrap();
        std::fs::write(target.join("index.html"), "same").unwrap();
        std::fs::write(target.join("assets/stale.js"), "stale").unwrap();
        let original_modified = std::fs::metadata(target.join("index.html"))
            .unwrap()
            .modified()
            .unwrap();

        commit_output_tree(&staging, &target).unwrap();
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            "same"
        );
        assert_eq!(
            std::fs::metadata(target.join("index.html"))
                .unwrap()
                .modified()
                .unwrap(),
            original_modified
        );
        assert_eq!(
            std::fs::read_to_string(target.join("assets/new.js")).unwrap(),
            "new"
        );
        assert!(!target.join("assets/stale.js").exists());
        commit_output_tree(&staging, &target).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn docs_output_commit_rolls_back_after_a_locked_stale_file() {
        use std::os::windows::fs::OpenOptionsExt;

        let fixture = Fixture::new("docs-commit-rollback");
        let staging = fixture.0.join("stage");
        let target = fixture.0.join("docs-dist");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(staging.join("a.txt"), "new-a").unwrap();
        std::fs::write(target.join("a.txt"), "old-a").unwrap();
        std::fs::write(target.join("z-stale.txt"), "old-z").unwrap();
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(target.join("z-stale.txt"))
            .unwrap();

        let error = commit_output_tree(&staging, &target).unwrap_err();
        assert_eq!(error.code, "WAKE_IO");
        assert_eq!(
            std::fs::read_to_string(target.join("a.txt")).unwrap(),
            "old-a"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("z-stale.txt")).unwrap(),
            "old-z"
        );
    }

    #[test]
    fn build_bundle_and_context_share_the_application_layer() {
        let fixture = Fixture::new("build");
        let options = BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist-node")),
            ..BuildOptions::default()
        };
        let result = build(options.clone(), &CancellationToken::default()).unwrap();
        assert!(result.success);
        assert!(result.files.iter().any(|file| file.kind == "html"));

        let bundled = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(!bundled.code.is_empty());
        assert!(bundled.output_file.is_none());

        let context = BuildContext::create(options).unwrap();
        let first = context.clone();
        let second = context.clone();
        let first = thread::spawn(move || first.rebuild(Vec::new(), CancellationToken::default()));
        let second =
            thread::spawn(move || second.rebuild(Vec::new(), CancellationToken::default()));
        assert!(first.join().unwrap().unwrap().success);
        assert!(second.join().unwrap().unwrap().success);

        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert_eq!(
            context.rebuild(Vec::new(), cancelled).unwrap_err().code,
            "WAKE_CANCELLED"
        );
        let first_close = context.clone();
        let second_close = context.clone();
        let first_close = thread::spawn(move || first_close.close());
        let second_close = thread::spawn(move || second_close.close());
        first_close.join().unwrap();
        second_close.join().unwrap();
        context.close();
        assert_eq!(
            context
                .rebuild(Vec::new(), CancellationToken::default())
                .unwrap_err()
                .code,
            "WAKE_INTERNAL"
        );
    }

    #[test]
    fn build_errors_include_numbered_source_location_data() {
        let fixture = Fixture::new("diagnostic-location");
        fixture.write(
            "src/index.js",
            "const first = 1;\nconst second = 2;\nconst broken = ;\n",
        );
        let error = build(
            BuildOptions {
                project: fixture.project(),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        let diagnostic = error
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == "error")
            .expect("parse diagnostic");
        let location = diagnostic.location.as_ref().expect("source location");
        assert_eq!(location.line, 3);
        assert_eq!(location.line_text, "const broken = ;");
        assert!(location.column > 1);
    }

    #[test]
    fn node_bundle_writes_only_the_requested_commonjs_file() {
        let fixture = Fixture::new("node-bundle");
        let output_dir = fixture.0.join("artifacts");
        std::fs::create_dir_all(&output_dir).unwrap();
        let sibling = output_dir.join("keep.txt");
        std::fs::write(&sibling, "keep").unwrap();
        std::fs::write(output_dir.join("extension.js"), "stale").unwrap();
        let result = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                outfile: Some(PathBuf::from("artifacts/extension.js")),
                platform: Some(BuildPlatform::Node),
                format: None,
                target: Some("node20".to_string()),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        let outfile = output_dir.join("extension.js");
        assert_same_existing_file(
            result.output_file.as_deref().expect("bundle output path"),
            &outfile,
        );
        assert!(
            std::fs::read_to_string(&outfile)
                .unwrap()
                .contains("module.exports = __wake_entry__")
        );
        assert_eq!(std::fs::read_to_string(&sibling).unwrap(), "keep");
        assert!(!output_dir.join("index.html").exists());
        assert!(!output_dir.join("manifest.json").exists());
    }

    #[test]
    fn browser_bundle_defaults_preserve_iife_and_css_runtime_behavior() {
        let fixture = Fixture::new("browser-bundle");
        fixture.write(
            "src/index.js",
            "import './theme.css'; export const value = 42;\n",
        );
        fixture.write("src/theme.css", "body { color: rebeccapurple; }\n");

        let result = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();

        assert!(result.code.contains("rebeccapurple"), "{}", result.code);
        assert!(result.code.contains("__wake_entry__"), "{}", result.code);
        assert!(result.output_file.is_none());
    }

    #[test]
    fn browser_exact_outfile_inlines_assets_instead_of_emitting_siblings() {
        let fixture = Fixture::new("browser-exact-outfile");
        fixture.write(
            "src/index.js",
            "import image from './large.png'; export default image;\n",
        );
        fixture.write("src/large.png", vec![b'X'; 8 * 1024]);

        let result = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                outfile: Some(PathBuf::from("artifacts/browser.js")),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();

        assert!(result.code.contains("data:image/png;base64,"));
        assert_eq!(result.files.len(), 1);
        assert_eq!(
            std::fs::read_dir(fixture.0.join("artifacts"))
                .unwrap()
                .filter_map(Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn bundle_source_map_is_returned_and_written_next_to_exact_outfile() {
        let fixture = Fixture::new("bundle-source-map");
        let memory = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(memory.source_map.is_some());
        assert!(memory.source_map_file.is_none());
        assert!(!memory.code.contains("sourceMappingURL="));
        assert!(memory.files.iter().any(|file| file.kind == "map"));

        let written = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                outfile: Some(PathBuf::from("artifacts/extension.js")),
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        let outfile = fixture.0.join("artifacts/extension.js");
        let mapfile = fixture.0.join("artifacts/extension.js.map");
        assert_same_existing_file(
            written
                .source_map_file
                .as_deref()
                .expect("bundle source map path"),
            &mapfile,
        );
        let disk_map = std::fs::read_to_string(&mapfile).unwrap();
        assert_eq!(written.source_map.as_deref(), Some(disk_map.as_str()));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&disk_map).unwrap()["file"],
            "extension.js"
        );
        let code = std::fs::read_to_string(outfile).unwrap();
        assert_eq!(written.code, code);
        assert!(code.ends_with("//# sourceMappingURL=extension.js.map\n"));
    }

    #[test]
    fn bundle_minify_with_source_map_is_returned_and_written() {
        let fixture = Fixture::new("bundle-minified-source-map");
        fixture.write(
            "src/index.js",
            "function compute(descriptiveParameter) { const foldedValue = 1 + 2; return descriptiveParameter + foldedValue; } export const value = compute(4);",
        );

        let memory = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                minify: true,
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(memory.source_map.is_some());
        assert!(
            !memory.code.contains("descriptiveParameter"),
            "{}",
            memory.code
        );
        assert!(!memory.code.contains("sourceMappingURL="));

        let written = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("src/index.js")),
                outfile: Some(PathBuf::from("artifacts/minified.js")),
                minify: true,
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(written.source_map.is_some());
        assert!(
            written
                .code
                .ends_with("//# sourceMappingURL=minified.js.map\n")
        );
        assert!(fixture.0.join("artifacts/minified.js.map").is_file());
    }

    #[test]
    fn bundle_option_validation_is_owned_by_the_application_layer() {
        let fixture = Fixture::new("bundle-validation");
        let invalid_pair = bundle(
            BundleOptions {
                project: fixture.project(),
                platform: Some(BuildPlatform::Node),
                format: Some(ModuleFormat::Iife),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(invalid_pair.code, "WAKE_CONFIG");

        for external in ["./local", "pkg/*", "pkg name", "@scope", "pkg/subpath"] {
            let error = bundle(
                BundleOptions {
                    project: fixture.project(),
                    external: vec![external.to_string()],
                    ..BundleOptions::default()
                },
                &CancellationToken::default(),
            )
            .unwrap_err();
            assert_eq!(error.code, "WAKE_CONFIG", "{external}");
        }
    }

    #[test]
    fn atomic_write_concurrently_replaces_with_one_complete_payload() {
        let fixture = Fixture::new("atomic-write");
        let target = fixture.0.join("artifacts/extension.js");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let first_barrier = barrier.clone();
        let first_target = target.clone();
        let first = thread::spawn(move || {
            first_barrier.wait();
            atomic_write(&first_target, &vec![b'A'; 128 * 1024])
        });
        let second_barrier = barrier.clone();
        let second_target = target.clone();
        let second = thread::spawn(move || {
            second_barrier.wait();
            atomic_write(&second_target, &vec![b'B'; 128 * 1024])
        });
        barrier.wait();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();

        let contents = std::fs::read(&target).unwrap();
        assert!(contents == vec![b'A'; 128 * 1024] || contents == vec![b'B'; 128 * 1024]);
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".wake-bundle-"))
                .count(),
            0
        );
    }

    #[test]
    fn dev_server_close_is_idempotent_and_releases_its_port() {
        let fixture = Fixture::new("dev");
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let server = start_dev_server(DevServerOptions {
            project: fixture.project(),
            port: Some(port),
            ..DevServerOptions::default()
        })
        .unwrap();
        assert_eq!(server.url(), format!("http://127.0.0.1:{port}/"));
        let initial_events = server.drain_events();
        assert!(initial_events.iter().any(|event| matches!(
            event,
            DevServerEvent::Rebuilt {
                initial: true,
                modules,
                updated_modules: _,
                chunks,
                duration_ms,
                ..
            } if *modules > 0 && *chunks > 0 && *duration_ms >= 0.0
        )));
        let closing = server.clone();
        let waiting = server.clone();
        let closing = thread::spawn(move || closing.close());
        let waiting = thread::spawn(move || waiting.wait_until_closed());
        closing.join().unwrap().unwrap();
        waiting.join().unwrap().unwrap();
        server.close().unwrap();
        assert!(
            server
                .drain_events()
                .iter()
                .any(|event| matches!(event, DevServerEvent::Closed))
        );
        let rebound = TcpListener::bind(("127.0.0.1", port)).unwrap();
        drop(rebound);
    }

    #[test]
    fn dev_server_events_keep_structured_source_diagnostics() {
        let fixture = Fixture::new("dev-diagnostic");
        fixture.write(
            "src/index.js",
            "const first = 1;\nconst second = 2;\nconst broken = ;\n",
        );
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let server = start_dev_server(DevServerOptions {
            project: fixture.project(),
            port: Some(port),
            ..DevServerOptions::default()
        })
        .unwrap();
        let events = server.drain_events();
        let json = serde_json::to_value(&events).unwrap();
        assert!(json.as_array().unwrap().iter().any(|event| {
            event["type"] == "diagnostic"
                && event["diagnostic"]["location"]["line"] == 3
                && event["diagnostic"]["location"]["lineText"] == "const broken = ;"
        }));
        let diagnostic = events
            .iter()
            .find_map(|event| match event {
                DevServerEvent::Diagnostic { diagnostic } => Some(diagnostic),
                _ => None,
            })
            .expect("structured diagnostic event");
        assert_eq!(diagnostic.severity, "error");
        assert_eq!(
            diagnostic.location.as_ref().map(|location| location.line),
            Some(3)
        );
        assert_eq!(
            diagnostic
                .location
                .as_ref()
                .map(|location| location.line_text.as_str()),
            Some("const broken = ;")
        );
        server.close().unwrap();
    }

    #[test]
    fn dev_server_bind_failure_returns_without_leaving_a_watcher() {
        let fixture = Fixture::new("bind-failure");
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        let error = match start_dev_server(DevServerOptions {
            project: fixture.project(),
            port: Some(port),
            ..DevServerOptions::default()
        }) {
            Ok(server) => {
                let _ = server.close();
                panic!("occupied port unexpectedly accepted")
            }
            Err(error) => error,
        };
        assert_eq!(error.code, "WAKE_IO");
        drop(reservation);
    }
}
