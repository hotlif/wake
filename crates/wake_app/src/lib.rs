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
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

use ring::digest::{Context as DigestContext, SHA256};
use serde::{Deserialize, Serialize};
use wake_bundler::{
    BuildGeneration, BuildOptions as BundlerBuildOptions, BuildOutput, BuildRequest, BuildSession,
    FederationBuildPlan, FederationEntryExport, JsxOptions, ResolveOptions,
};
pub use wake_bundler::{BuildPlatform, ModuleFormat};
use wake_common::{
    Diagnostic, FileSystem, OsFileSystem, OwnedFileTree, OwnedFileTreeBuilder,
    OwnedOverlayFileSystem, ProjectedRelativePath, SourceFile,
};

pub use wake_config::{
    ContainerName, ExposeConfig, ExposeKey, ExposeMode, FederationOptions, RemoteConfig,
    ShadowMode, SharedConfig,
};
pub use wake_dev_server::{
    WatchInterest, WatchInvalidation, WatchReconcileError, WatchReconcileOutcome,
    WatchRegistrationState, WatchTreeFilter, reconcile_watch_interests,
};
pub use wake_docs::{DocsMode, DocsPresentation};
use wake_ecma_transform::{BrowserTarget, TargetEnv};
pub use wake_test_contract::protocol::WatchControl as TestWatchControl;
use wake_test_contract::protocol::{
    FrameDecoder, HOST_BUILD_ID, HostAck, HostCommand, HostError, HostEvent, HostHello,
    HostRequest, HostResponse, HostResponseBody, PROTOCOL_VERSION, WatchControl, write_frame,
};
pub use wake_test_contract::{
    TestCaseResult, TestDiagnostic, TestEnvironmentKind, TestFailure, TestLeakKind, TestOptions,
    TestRunResult, TestStatus, TestSuiteResult, TestSuiteStatus, TestTerminationReason,
    WorkerOverride,
};

mod federation;
mod federation_init;
mod federation_lock;
mod federation_type_sync;
mod federation_type_watch;
mod federation_types;
mod library;
mod output;
pub use federation_init::{
    FederationInitFileStatus, FederationInitResult, initialize_federation_types,
};
pub use federation_lock::{
    federation_project_root, generate_federation_lock, generate_project_federation_lock,
};
pub use federation_type_sync::{
    FederationTypeSyncResult, SyncedFederationTypes, sync_federation_types,
};
pub use library::{
    GenerateCssTokenOptions, GenerateCssTokenResult, GenerateDocgenOptions, GenerateDocgenResult,
    LibraryBuildOptions, LibraryBuildResult, build_library, generate_css_token, generate_docgen,
};
use output::{
    ExactOutput, RecordingFileSystem, acquire_output_commit_lock, is_output_commit_lock_path,
    publish_exact_outputs,
};
pub use output::{OutputFile, OutputFileKind};

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

fn diagnostic_infos(
    diagnostics: &[Diagnostic],
    root: &Path,
    file_system: &dyn FileSystem,
) -> Vec<DiagnosticInfo> {
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
                        file_system
                            .read_to_string(&resolved)
                            .ok()
                            .map(|text| SourceFile::new(path, text))
                    })
                    .as_ref()
            });
            DiagnosticInfo::from_diagnostic(diagnostic, source)
        })
        .collect()
}

fn diagnostic_infos_from_captured_sources(
    diagnostics: &[Diagnostic],
    captured: Vec<wake_dev_server::DiagnosticSource>,
) -> Vec<DiagnosticInfo> {
    let sources = captured
        .into_iter()
        .map(|captured| {
            let path = captured.path.to_string_lossy().into_owned();
            let source = SourceFile::new(path.clone(), captured.text);
            (path, source)
        })
        .collect::<HashMap<_, _>>();
    diagnostics
        .iter()
        .map(|diagnostic| {
            let source = diagnostic
                .path
                .as_deref()
                .and_then(|path| sources.get(path));
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
    /// Programmatic federation override. `None` uses `wake.config.toml`.
    pub federation: Option<FederationOptions>,
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
            federation: None,
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

#[derive(Debug, Default)]
struct CancellationState {
    cancelled: AtomicBool,
    commit_gate: RwLock<()>,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<CancellationState>);

impl CancellationToken {
    pub fn cancel(&self) {
        // Publish cancellation before waiting for an in-flight commit. This prevents a second
        // operation from entering through an implementation-dependent reader preference while a
        // cancellation writer is queued. A commit which already passed the read-side fence
        // linearizes first; cancel() still waits for it to leave before returning.
        self.0.cancelled.store(true, Ordering::Release);
        let _gate = self
            .0
            .commit_gate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    fn check(&self) -> Result<(), WakeError> {
        if self.is_cancelled() {
            Err(WakeError::cancelled())
        } else {
            Ok(())
        }
    }

    fn commit<T>(&self, commit: impl FnOnce() -> Result<T, WakeError>) -> Result<T, WakeError> {
        let _gate = self
            .0
            .commit_gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.check()?;
        commit()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ControlFingerprint {
    Present { len: u64, sha256: [u8; 32] },
    Unreadable(std::io::ErrorKind),
}

type ControlFileFingerprint = (PathBuf, ControlFingerprint);

#[derive(Clone)]
struct PreparedBuild {
    config_dir: PathBuf,
    root: PathBuf,
    entry: PathBuf,
    logical_entry: PathBuf,
    explicit_entry: Option<PathBuf>,
    outdir: PathBuf,
    config: wake_config::Config,
    control_fingerprints: Vec<ControlFileFingerprint>,
    aliases: Vec<(String, PathBuf)>,
    core_generation: GenerationView,
    generation: GenerationView,
}

#[derive(Clone)]
struct PreparedBuildProbe {
    config_dir: PathBuf,
    root: PathBuf,
    logical_entry: PathBuf,
    explicit_entry: Option<PathBuf>,
    outdir: PathBuf,
    config: wake_config::Config,
    control_fingerprints: Vec<ControlFileFingerprint>,
}

/// The only mutable phase of a generated-input generation.
///
/// A draft is deliberately not cloneable and exposes no filesystem view. Every generated byte
/// must be inserted before [`Self::seal`] transfers ownership to an immutable [`GenerationView`].
struct GenerationDraft {
    project_root: PathBuf,
    files: OwnedFileTreeBuilder,
}

impl GenerationDraft {
    fn new(project_root: &Path) -> Self {
        Self {
            project_root: project_root.to_path_buf(),
            files: OwnedFileTreeBuilder::new(),
        }
    }

    fn write_file(
        &mut self,
        relative: impl AsRef<Path>,
        contents: impl Into<Arc<[u8]>>,
    ) -> Result<PathBuf, WakeError> {
        let relative = ProjectedRelativePath::new(relative.as_ref())
            .map_err(|error| generated_input_error(&self.project_root, relative.as_ref(), error))?;
        let logical = self.logical_path(&relative);
        self.files
            .insert(relative, contents)
            .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()).at(&logical))?;
        Ok(logical)
    }

    fn insert_tree(
        &mut self,
        prefix: impl AsRef<Path>,
        tree: &OwnedFileTree,
    ) -> Result<(), WakeError> {
        let prefix = prefix.as_ref();
        for (relative, contents) in tree.iter() {
            self.write_file(prefix.join(relative.as_path()), Arc::clone(contents))?;
        }
        Ok(())
    }

    fn logical_path(&self, relative: &ProjectedRelativePath) -> PathBuf {
        self.project_root.join(".wake").join(relative.as_path())
    }

    fn seal(self) -> Result<GenerationView, WakeError> {
        let (project_root, files) = self.finish();
        GenerationView::from_tree(project_root, files)
    }

    fn finish(self) -> (PathBuf, OwnedFileTree) {
        (self.project_root, self.files.seal())
    }
}

/// Immutable owner of one complete logical `.wake` generated-input generation.
///
/// Clones retain exactly the same byte tree and filesystem capability. There is intentionally no
/// mutation API: adding another producer creates a new view before any bundler session observes it.
#[derive(Clone)]
struct GenerationView {
    inner: Arc<GenerationViewInner>,
}

struct GenerationViewInner {
    project_root: PathBuf,
    files: OwnedFileTree,
    file_system: Arc<dyn FileSystem>,
}

#[cfg(test)]
static GENERATION_SEALS: std::sync::LazyLock<Mutex<HashMap<PathBuf, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

impl GenerationView {
    fn from_tree(project_root: PathBuf, files: OwnedFileTree) -> Result<Self, WakeError> {
        let logical_root = project_root.join(".wake");
        let file_system: Arc<dyn FileSystem> = Arc::new(
            OwnedOverlayFileSystem::try_new(Arc::new(OsFileSystem), &logical_root, files.clone())
                .map_err(|error| {
                WakeError::new("WAKE_INTERNAL", error.to_string()).at(&logical_root)
            })?,
        );
        #[cfg(test)]
        {
            let mut seals = GENERATION_SEALS
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *seals.entry(project_root.clone()).or_default() += 1;
        }
        Ok(Self {
            inner: Arc::new(GenerationViewInner {
                project_root,
                files,
                file_system,
            }),
        })
    }

    fn file_system(&self) -> Arc<dyn FileSystem> {
        Arc::clone(&self.inner.file_system)
    }

    fn logical_path(&self, relative: &ProjectedRelativePath) -> PathBuf {
        self.inner
            .project_root
            .join(".wake")
            .join(relative.as_path())
    }

    fn logical_inventory(&self) -> Vec<PathBuf> {
        self.inner
            .files
            .inventory()
            .map(|relative| self.logical_path(relative))
            .collect()
    }

    fn owns_logical_file(&self, path: &Path) -> bool {
        path.strip_prefix(self.inner.project_root.join(".wake"))
            .ok()
            .and_then(|relative| ProjectedRelativePath::new(relative).ok())
            .is_some_and(|relative| self.inner.files.get(&relative).is_some())
    }

    fn has_same_files(&self, other: &Self) -> bool {
        self.has_same_tree(&other.inner.files)
    }

    fn has_same_tree(&self, other: &OwnedFileTree) -> bool {
        self.inner.files.len() == other.len()
            && self.inner.files.iter().all(|(path, contents)| {
                other
                    .get(path)
                    .is_some_and(|candidate| candidate == contents.as_ref())
            })
    }

    #[cfg(test)]
    fn is_same_generation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

fn generated_input_error(
    _project_root: &Path,
    relative: &Path,
    error: wake_common::OwnedFileTreeError,
) -> WakeError {
    WakeError::new(
        "WAKE_INTERNAL",
        format!("invalid generated input `{}`: {error}", relative.display()),
    )
}

impl PreparedBuild {
    /// Replace the final product view with `core_generation + additional` in one sealed tree.
    /// Reinstalling byte-identical inputs preserves the existing view identity and any session
    /// that already owns it.
    fn install_product_inputs(&mut self, additional: &OwnedFileTree) -> Result<bool, WakeError> {
        let candidate = if additional.is_empty() {
            self.core_generation.clone()
        } else {
            let mut draft = GenerationDraft::new(&self.core_generation.inner.project_root);
            draft.insert_tree(Path::new(""), &self.core_generation.inner.files)?;
            draft.insert_tree(Path::new(""), additional)?;
            draft.seal()?
        };
        if self.generation.has_same_files(&candidate) {
            return Ok(false);
        }
        self.generation = candidate;
        Ok(true)
    }
}

fn generation_changed_paths(previous: &GenerationView, next: &GenerationView) -> Vec<PathBuf> {
    let mut changed = BTreeSet::new();
    for (path, contents) in previous.inner.files.iter() {
        if next.inner.files.get(path) != Some(contents.as_ref()) {
            changed.insert(previous.logical_path(path));
        }
    }
    for (path, contents) in next.inner.files.iter() {
        if previous.inner.files.get(path) != Some(contents.as_ref()) {
            changed.insert(next.logical_path(path));
        }
    }
    changed.into_iter().collect()
}

enum CandidateState<T, E = Diagnostic> {
    Stable,
    Pending {
        id: u64,
        draft: T,
        last_error: Option<E>,
    },
    Blocked {
        diagnostic: E,
    },
}

struct RefreshState<A, C, E = Diagnostic> {
    accepted: A,
    next_id: u64,
    candidate: CandidateState<C, E>,
}

#[derive(Clone)]
struct PreparedDocsRefresh {
    prepared: PreparedBuild,
    docs: wake_docs::DocsOptions,
    warnings: Vec<String>,
}

#[derive(Clone)]
struct PreparedDocsProbe {
    prepared: PreparedBuildProbe,
    docs: wake_docs::DocsOptions,
}

fn docs_probe_from_refresh(prepared: &PreparedDocsRefresh) -> PreparedDocsProbe {
    PreparedDocsProbe {
        prepared: build_probe_from_prepared(&prepared.prepared),
        docs: prepared.docs.clone(),
    }
}

const OUTPUT_OWNERSHIP_FILE: &str = ".wake-output.json";
const OUTPUT_OWNERSHIP_SCHEMA: &str = "wake.output.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputProduct {
    Application,
    Documentation,
}

impl OutputProduct {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Documentation => "documentation",
        }
    }

    const fn backup_prefix(self) -> &'static str {
        match self {
            Self::Application => ".wake-app-backup-",
            Self::Documentation => ".wake-docs-backup-",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OutputOwnership {
    schema_version: String,
    owner: String,
    product: String,
}

impl OutputOwnership {
    fn wake(product: OutputProduct) -> Self {
        Self {
            schema_version: OUTPUT_OWNERSHIP_SCHEMA.to_string(),
            owner: "wake".to_string(),
            product: product.as_str().to_string(),
        }
    }
}

type PreparedDocs = (
    PreparedBuild,
    wake_docs::DocsOptions,
    Vec<wake_docs::RouteInfo>,
    Vec<wake_docs::DemoDescriptor>,
    Vec<String>,
    Vec<PathBuf>,
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
    let mut prepared = prepare_build(&options)?;
    cancellation.check()?;
    let bundler_options = create_bundler_options(&prepared, &options, project_defaults)?;
    let federation_inputs = federation::render_production_inputs(&prepared, &options)?;
    prepared.install_product_inputs(federation_inputs.files())?;
    let mut generation = BuildGeneration::new(prepared.generation.file_system());
    let federation_generation = federation::bind_production_generation(
        &prepared,
        &options,
        &federation_inputs,
        generation.file_system_view(),
    )?;
    cancellation.check()?;
    let request = BuildRequest::new(&prepared.entry);
    let output = generation.build_once(bundler_options, request);
    cancellation.check()?;
    finish_output(
        &prepared,
        &options,
        output,
        started,
        federation_generation,
        &mut generation,
        cancellation,
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
    if let Some(outfile) = options.outfile.as_deref() {
        validate_not_reserved(
            &prepared.root,
            "Bundle output",
            &absolute_from(&prepared.root, outfile),
        )?;
    }
    let phase_started = Instant::now();
    let bundler_options = create_bundle_options(&prepared, &options)?;
    let recording_fs = RecordingFileSystem::new(prepared.generation.file_system());
    let mut generation = BuildGeneration::new(Arc::new(recording_fs.clone()));
    let create_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
    let request = BuildRequest::new(&prepared.entry);
    let output = generation.build_once(bundler_options, request);
    cancellation.check()?;
    let build_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
    // Owned generated inputs are immutable bytes in the GenerationView, not host files. Physical
    // identity validation must not canonicalize them; doing so would report that the deliberately
    // absent `.wake` projection disappeared. The reserved-output check above separately prevents
    // an exact output from targeting the logical generated-input namespace.
    let mut protected_inputs = recording_fs
        .inputs()
        .into_iter()
        .filter(|path| !prepared.generation.owns_logical_file(path))
        .collect::<Vec<_>>();
    if !prepared.generation.owns_logical_file(&prepared.entry) {
        protected_inputs.push(prepared.entry.clone());
    }
    for path in [
        prepared.config_dir.join(wake_config::CONFIG_FILE),
        prepared.root.join(".browserslistrc"),
        prepared.root.join("package.json"),
    ] {
        if path.is_file() {
            protected_inputs.push(path);
        }
    }
    let mut result = finish_bundle(
        &prepared,
        &options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
        generation.file_system_view().as_ref(),
        &protected_inputs,
        cancellation,
    )?;
    let finish_elapsed = phase_started.elapsed();
    let phase_started = Instant::now();
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
    prepare_build_with_generation(options, false)
}

fn prepare_build_candidate(options: &BuildOptions) -> Result<PreparedBuild, WakeError> {
    prepare_build_with_generation(options, true)
}

fn prepare_build_with_generation(
    options: &BuildOptions,
    candidate_generation: bool,
) -> Result<PreparedBuild, WakeError> {
    materialize_build_probe(probe_build_candidate(options)?, candidate_generation)
}

fn probe_build_candidate(options: &BuildOptions) -> Result<PreparedBuildProbe, WakeError> {
    let mut last_snapshot_error = None;
    for _ in 0..3 {
        match probe_build_candidate_once(options) {
            Err(error) if error.code == "WAKE_WATCH_SNAPSHOT_CHANGED" => {
                last_snapshot_error = Some(error);
            }
            result => return result,
        }
    }
    Err(last_snapshot_error.unwrap_or_else(|| {
        WakeError::new(
            "WAKE_WATCH_SNAPSHOT_CHANGED",
            "project control files changed while preparing the build",
        )
    }))
}

fn probe_build_candidate_once(options: &BuildOptions) -> Result<PreparedBuildProbe, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config_path = config_dir.join(wake_config::CONFIG_FILE);
    let config_before = control_file_fingerprint(&config_path);
    let mut config = wake_config::load(&config_dir)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&config_path))?;
    if let Some(federation) = &options.federation {
        config.federation = federation
            .clone()
            .validate_and_normalize()
            .map_err(|error| {
                WakeError::new("FED_CONFIG_INVALID", error.to_string()).at(&config_dir)
            })?;
    }
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
    validate_reserved_build_inputs(&config, &root, options.entry.as_deref())?;
    let (explicit_entry, logical_entry) = match &options.entry {
        Some(entry) => {
            let entry = absolute_from(&root, entry);
            (Some(entry.clone()), entry)
        }
        None => (None, virtual_entry_target(&root, &config)),
    };
    let outdir = absolute_from_project_root(
        &configured_root,
        &root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("dist")),
    );
    let control_fingerprints = stable_project_control_snapshot(
        &config_before,
        &config_dir,
        &root,
        explicit_entry.as_deref(),
    )?;
    Ok(PreparedBuildProbe {
        config_dir,
        root,
        logical_entry,
        explicit_entry,
        outdir,
        config,
        control_fingerprints,
    })
}

fn materialize_build_probe(
    probe: PreparedBuildProbe,
    _candidate_generation: bool,
) -> Result<PreparedBuild, WakeError> {
    let PreparedBuildProbe {
        config_dir,
        root,
        logical_entry,
        explicit_entry,
        outdir,
        config,
        control_fingerprints,
    } = probe;
    let mut generation = GenerationDraft::new(&root);
    let aliases = prepare_generation_aliases(&config, &root, &mut generation)?;
    let entry = match &explicit_entry {
        Some(entry) => {
            if !entry.is_file() {
                return Err(WakeError::new(
                    "WAKE_IO",
                    format!("entry file does not exist: {}", entry.display()),
                )
                .at(entry));
            }
            entry
                .canonicalize()
                .map(|entry| wake_common::fs::normalize(&entry))
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(entry))?
        }
        None => virtual_entry_in(&config, &mut generation)?,
    };
    let generation = generation.seal()?;
    Ok(PreparedBuild {
        config_dir,
        root,
        entry,
        logical_entry,
        explicit_entry,
        outdir,
        config,
        control_fingerprints,
        aliases,
        core_generation: generation.clone(),
        generation,
    })
}

fn build_probe_from_prepared(prepared: &PreparedBuild) -> PreparedBuildProbe {
    PreparedBuildProbe {
        config_dir: prepared.config_dir.clone(),
        root: prepared.root.clone(),
        logical_entry: prepared.logical_entry.clone(),
        explicit_entry: prepared.explicit_entry.clone(),
        outdir: prepared.outdir.clone(),
        config: prepared.config.clone(),
        control_fingerprints: prepared.control_fingerprints.clone(),
    }
}

fn validate_reserved_build_inputs(
    config: &wake_config::Config,
    root: &Path,
    explicit_entry: Option<&Path>,
) -> Result<(), WakeError> {
    let mut inputs = Vec::<(&str, PathBuf)>::new();
    let entry = explicit_entry
        .map(|entry| absolute_from(root, entry))
        .unwrap_or_else(|| virtual_entry_target(root, config));
    inputs.push(("entry", entry));
    inputs.extend(
        config
            .alias
            .values()
            .map(|path| ("resolver alias", absolute_from(root, Path::new(path)))),
    );
    inputs.extend(config.component_scan.iter().map(|rule| {
        (
            "component scan root",
            absolute_from(root, Path::new(&rule.cwd)),
        )
    }));
    inputs.extend(config.federation.exposes.values().map(|expose| {
        (
            "Federation expose",
            absolute_from(root, Path::new(&expose.entry)),
        )
    }));
    for (kind, path) in inputs {
        validate_not_reserved(root, kind, &path)?;
    }
    Ok(())
}

fn validate_not_reserved(root: &Path, kind: &str, path: &Path) -> Result<(), WakeError> {
    let reserved = wake_common::fs::normalize(&root.join(".wake"));
    let resolved_reserved = reserved
        .canonicalize()
        .map(|path| wake_common::fs::normalize(&path))
        .unwrap_or_else(|_| reserved.clone());
    let path = wake_common::fs::normalize(path);
    let resolved = path
        .canonicalize()
        .map(|path| wake_common::fs::normalize(&path))
        .unwrap_or_else(|_| path.clone());
    if path.starts_with(&reserved) || resolved.starts_with(&resolved_reserved) {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("{kind} must not point into Wake's reserved `.wake` directory"),
        )
        .at(&path));
    }
    Ok(())
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

fn prepare_aliases_and_scans_in(
    config: &wake_config::Config,
    root: &Path,
    generation: &mut GenerationDraft,
) -> Result<Vec<(String, PathBuf)>, WakeError> {
    let mut aliases = config.resolver_aliases(root);
    if config.component_scan.is_empty() {
        return Ok(aliases);
    }
    for (index, rule) in config.component_scan.iter().enumerate() {
        let source = wake_scan::scan(&wake_scan::ScanRule {
            namespace: &rule.namespace,
            scan_dir: &root.join(&rule.cwd),
            root,
            generate_source: rule.generate_source,
            include: rule.include.as_deref(),
            exclude: rule.exclude.as_deref(),
        })
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
        let file = generation.write_file(
            PathBuf::from("scan").join(format!(
                "{index:04}-{}.ts",
                sanitize_namespace(&rule.namespace)
            )),
            source.as_bytes(),
        )?;
        aliases.push((format!("@@@/{}", rule.namespace), file));
    }
    Ok(aliases)
}

fn prepare_generation_aliases(
    config: &wake_config::Config,
    root: &Path,
    generation: &mut GenerationDraft,
) -> Result<Vec<(String, PathBuf)>, WakeError> {
    prepare_aliases_and_scans_in(config, root, generation)
}

/// Re-render application-owned virtual inputs after watcher coverage is installed.
///
/// Returning `true` means an already-created bundler session cannot be reused because its source
/// overlay owns a different immutable file tree.
fn refresh_application_generation(prepared: &mut PreparedBuild) -> Result<bool, WakeError> {
    let mut draft = GenerationDraft::new(&prepared.root);
    let aliases = prepare_generation_aliases(&prepared.config, &prepared.root, &mut draft)?;
    let entry = match &prepared.explicit_entry {
        Some(entry) => entry.clone(),
        None => virtual_entry_in(&prepared.config, &mut draft)?,
    };
    let (project_root, files) = draft.finish();
    let changed = !prepared.core_generation.has_same_tree(&files);
    prepared.aliases = aliases;
    prepared.entry = entry;
    if changed {
        let generation = GenerationView::from_tree(project_root, files)?;
        prepared.core_generation = generation.clone();
        prepared.generation = generation;
    }
    Ok(changed)
}

type ComponentScanTopology = (String, String, bool, Option<String>, Option<String>);
type ProxyTopology = (Vec<String>, String, bool, bool, Vec<(String, String)>);
type DocsServerTopology = (Option<String>, u16, String, bool, Vec<ProxyTopology>);
type DocsWorkspaceTopology = Vec<(String, String, String, &'static str, &'static str)>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct DevTopology {
    config_dir: PathBuf,
    root: PathBuf,
    entry: PathBuf,
    public_path: String,
    component_scan: Vec<ComponentScanTopology>,
    server_protocol: Option<String>,
    port: u16,
    host: String,
    open: bool,
    proxy: Vec<ProxyTopology>,
    federation: FederationOptions,
    federation_browser_target: Option<TargetEnv>,
}

fn dev_topology(
    prepared: &PreparedBuild,
    options: &DevServerOptions,
    default_port: u16,
    target_env: &TargetEnv,
) -> DevTopology {
    dev_topology_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        &prepared.config,
        options,
        default_port,
        target_env,
    )
}

fn dev_probe_topology(
    prepared: &PreparedBuildProbe,
    options: &DevServerOptions,
    default_port: u16,
    target_env: &TargetEnv,
) -> DevTopology {
    dev_topology_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        &prepared.config,
        options,
        default_port,
        target_env,
    )
}

fn dev_topology_from(
    config_dir: &Path,
    root: &Path,
    logical_entry: &Path,
    config: &wake_config::Config,
    options: &DevServerOptions,
    default_port: u16,
    target_env: &TargetEnv,
) -> DevTopology {
    let server = &config.dev_server;
    DevTopology {
        config_dir: config_dir.to_path_buf(),
        root: root.to_path_buf(),
        entry: logical_entry.to_path_buf(),
        public_path: config.public_path().to_owned(),
        component_scan: config
            .component_scan
            .iter()
            .map(|rule| {
                (
                    rule.namespace.clone(),
                    rule.cwd.clone(),
                    rule.generate_source,
                    rule.include.clone(),
                    rule.exclude.clone(),
                )
            })
            .collect(),
        server_protocol: server.server.clone(),
        port: options.port.or(server.port).unwrap_or(default_port),
        host: options
            .host
            .clone()
            .or_else(|| server.host.clone())
            .unwrap_or_else(|| "127.0.0.1".to_owned()),
        open: options.open.unwrap_or(server.open),
        proxy: server
            .proxy
            .iter()
            .map(|proxy| {
                (
                    proxy.context.clone(),
                    proxy.target.clone(),
                    proxy.ws,
                    proxy.change_origin,
                    proxy
                        .path_rewrite
                        .iter()
                        .map(|(pattern, replacement)| (pattern.clone(), replacement.clone()))
                        .collect(),
                )
            })
            .collect(),
        federation: config.federation.clone(),
        federation_browser_target: config.federation.enabled.then(|| target_env.clone()),
    }
}

fn dev_topology_change(current: &DevTopology, candidate: &DevTopology) -> Option<String> {
    if current.config_dir != candidate.config_dir {
        return Some("configuration source changed".to_owned());
    }
    if current.root != candidate.root {
        return Some("project root changed".to_owned());
    }
    if current.entry != candidate.entry {
        return Some("development entry changed".to_owned());
    }
    if current.public_path != candidate.public_path {
        return Some("public URL base changed".to_owned());
    }
    if current.component_scan != candidate.component_scan {
        return Some("component scan topology changed".to_owned());
    }
    if current.server_protocol != candidate.server_protocol
        || current.port != candidate.port
        || current.host != candidate.host
        || current.open != candidate.open
        || current.proxy != candidate.proxy
    {
        return Some("development server or proxy topology changed".to_owned());
    }
    if current.federation != candidate.federation
        || current.federation_browser_target != candidate.federation_browser_target
    {
        return Some("Federation topology changed".to_owned());
    }
    None
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BuildTopology {
    config_dir: PathBuf,
    root: PathBuf,
    entry: PathBuf,
    outdir: PathBuf,
    component_scan: Vec<ComponentScanTopology>,
    federation: FederationOptions,
}

fn build_topology(prepared: &PreparedBuild) -> BuildTopology {
    build_topology_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        &prepared.outdir,
        &prepared.config,
    )
}

fn build_probe_topology(prepared: &PreparedBuildProbe) -> BuildTopology {
    build_topology_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        &prepared.outdir,
        &prepared.config,
    )
}

fn build_topology_from(
    config_dir: &Path,
    root: &Path,
    logical_entry: &Path,
    outdir: &Path,
    config: &wake_config::Config,
) -> BuildTopology {
    BuildTopology {
        config_dir: config_dir.to_path_buf(),
        root: root.to_path_buf(),
        entry: logical_entry.to_path_buf(),
        outdir: outdir.to_path_buf(),
        component_scan: config
            .component_scan
            .iter()
            .map(|rule| {
                (
                    rule.namespace.clone(),
                    rule.cwd.clone(),
                    rule.generate_source,
                    rule.include.clone(),
                    rule.exclude.clone(),
                )
            })
            .collect(),
        federation: config.federation.clone(),
    }
}

fn build_topology_change(current: &BuildTopology, candidate: &BuildTopology) -> Option<String> {
    if current.config_dir != candidate.config_dir {
        return Some("configuration source changed".to_owned());
    }
    if current.root != candidate.root {
        return Some("project root changed".to_owned());
    }
    if current.entry != candidate.entry {
        return Some("build entry changed".to_owned());
    }
    if current.outdir != candidate.outdir {
        return Some("build output directory changed".to_owned());
    }
    if current.component_scan != candidate.component_scan {
        return Some("component scan topology changed".to_owned());
    }
    if current.federation != candidate.federation {
        return Some("Federation topology changed".to_owned());
    }
    None
}

fn restart_required_error(reason: impl Into<String>) -> WakeError {
    WakeError::new(
        wake_dev_server::DEV_RESTART_REQUIRED_CODE,
        format!("{}; restart the Wake watch process", reason.into()),
    )
}

fn union_watch_interests(left: &[WatchInterest], right: &[WatchInterest]) -> Vec<WatchInterest> {
    let mut interests = left.iter().chain(right).cloned().collect::<Vec<_>>();
    interests.sort();
    interests.dedup();
    interests
}

fn recovery_watch_interests(
    mut interests: Vec<WatchInterest>,
    root: &Path,
    error: &WakeError,
) -> Vec<WatchInterest> {
    if let Some(path) = error.path.as_deref() {
        let path = Path::new(path);
        let interest =
            if path.file_name().and_then(|name| name.to_str()) == Some(wake_config::CONFIG_FILE) {
                WatchInterest::exact_file(path)
            } else {
                WatchInterest::tree(path)
            };
        interests.push(interest.resolve_against(root));
    }
    interests.sort();
    interests.dedup();
    interests
}

fn app_dev_plan(
    prepared: &PreparedBuild,
    entry: PathBuf,
    aliases: Vec<(String, PathBuf)>,
) -> Result<wake_dev_server::DevMountPlan, WakeError> {
    Ok(wake_dev_server::DevMountPlan {
        entry,
        resolve_options: ResolveOptions {
            alias: aliases,
            conditions: ["browser", "development", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            ..ResolveOptions::default()
        },
        define: build_defines(&prepared.config, true),
        target_env: resolve_target_env(&prepared.config, &prepared.root)?,
        jsx_import_source: prepared.config.react.jsx_import_source.clone(),
        file_system: prepared.generation.file_system(),
    })
}

fn prepare_dev_refresh_candidate(
    mut prepared: PreparedBuild,
    options: &BuildOptions,
) -> Result<
    (
        PreparedBuild,
        wake_dev_server::DevMountPlan,
        Vec<WatchInterest>,
        Vec<PathBuf>,
    ),
    WakeError,
> {
    refresh_application_generation(&mut prepared)?;
    let captured_lock = if prepared.config.federation.enabled
        && prepared
            .config
            .federation
            .remotes
            .values()
            .any(|remote| !remote.dev_follow)
    {
        federation::load_production_lock(&prepared)?.map(Arc::new)
    } else {
        None
    };
    let dev_federation = federation::prepare_dev(
        &prepared,
        options,
        prepared.generation.file_system(),
        captured_lock,
    )?;
    prepared.install_product_inputs(&dev_federation.generated_inputs)?;
    let mut watch_interests = project_watch_interests(&prepared);
    extend_runtime_source_interests(&prepared, &mut watch_interests, &dev_federation.aliases);
    let mut aliases = prepared.aliases.clone();
    aliases.extend(dev_federation.aliases);
    let plan = app_dev_plan(&prepared, dev_federation.entry, aliases)?;
    let generated_paths = prepared.generation.logical_inventory();
    Ok((prepared, plan, watch_interests, generated_paths))
}

#[allow(clippy::result_large_err)]
fn make_dev_refresh_candidate(
    state: Arc<Mutex<RefreshState<PreparedBuild, PreparedBuildProbe>>>,
    id: u64,
    draft: PreparedBuildProbe,
    options: BuildOptions,
) -> wake_dev_server::DevMountCandidate {
    let preliminary_interests = probe_watch_interests(&draft);
    let accepted_slot = Arc::new(Mutex::new(None::<PreparedBuild>));
    let materialize_slot = Arc::clone(&accepted_slot);
    let materialize_state = Arc::clone(&state);
    let materialize_draft = draft.clone();
    wake_dev_server::DevMountCandidate::new(
        preliminary_interests,
        move || {
            let result = materialize_build_probe(materialize_draft, true)
                .and_then(|prepared| prepare_dev_refresh_candidate(prepared, &options));
            match result {
                Ok((prepared, plan, watch_interests, _generated_paths)) => {
                    let generated_paths = {
                        let state = materialize_state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        generation_changed_paths(&state.accepted.generation, &prepared.generation)
                    };
                    *materialize_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(prepared);
                    Ok(wake_dev_server::DevMountMaterialization {
                        plan,
                        watch_interests,
                        generated_paths,
                    })
                }
                Err(error) => {
                    let diagnostic = wake_error_diagnostic(error);
                    let mut state = materialize_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let CandidateState::Pending {
                        id: current_id,
                        last_error,
                        ..
                    } = &mut state.candidate
                        && *current_id == id
                    {
                        *last_error = Some(diagnostic.clone());
                    }
                    Err(diagnostic)
                }
            }
        },
        move |outcome| {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = matches!(
                &state.candidate,
                CandidateState::Pending { id: current_id, .. } if *current_id == id
            );
            if !current {
                return;
            }
            match outcome {
                wake_dev_server::RefreshOutcome::Committed => {
                    if let Some(accepted) = accepted_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        state.accepted = accepted;
                        state.candidate = CandidateState::Stable;
                    }
                }
                wake_dev_server::RefreshOutcome::Superseded => {
                    state.candidate = CandidateState::Stable;
                }
                wake_dev_server::RefreshOutcome::RetryableFailure
                | wake_dev_server::RefreshOutcome::Aborted => {}
            }
        },
    )
}

fn alias_watch_interest(path: PathBuf) -> WatchInterest {
    // An alias target can change shape while the server is alive (missing -> dotted directory,
    // file -> directory). A source tree matches the exact root in every shape, then promotes to a
    // recursive registration once a directory exists.
    WatchInterest::tree(path)
}

fn project_control_interests(prepared: &PreparedBuild) -> Vec<WatchInterest> {
    project_control_interests_from(
        &prepared.config_dir,
        &prepared.root,
        prepared.explicit_entry.as_deref(),
    )
}

fn project_control_paths(
    config_dir: &Path,
    root: &Path,
    explicit_entry: Option<&Path>,
) -> Vec<PathBuf> {
    const INSTALL_LOCKS: [&str; 3] = ["yarn.lock", "package-lock.json", "pnpm-lock.yaml"];

    let mut paths = vec![
        config_dir.join(wake_config::CONFIG_FILE),
        root.join(".browserslistrc"),
        root.join("package.json"),
        root.join(".pnp.cjs"),
        root.join("yarn.lock"),
        root.join("package-lock.json"),
        root.join("pnpm-lock.yaml"),
        root.join("wake-federation.lock"),
    ];
    if root.join(".pnp.cjs").is_file() {
        paths.push(root.join(".pnp.data.json"));
    }

    // Resolver discovery starts at the logical project entry. A user-supplied entry may live
    // outside the project root, so it contributes a second discovery chain. Generated `.wake`
    // namespaces never become control owners.
    let mut discovery_roots = vec![root];
    if let Some(parent) = explicit_entry.and_then(Path::parent)
        && parent != root
    {
        discovery_roots.push(parent);
    }
    for start in discovery_roots {
        let pnp_root = start
            .ancestors()
            .find(|ancestor| ancestor.join(".pnp.cjs").is_file());
        let install_root = start.ancestors().find(|ancestor| {
            INSTALL_LOCKS
                .iter()
                .any(|name| ancestor.join(name).is_file())
        });
        let discovery_boundary = pnp_root.or(install_root).unwrap_or(start);
        // Match resolver PnP discovery: every closer `.pnp.cjs` marker can replace the currently
        // selected manifest, but sibling data/lock files matter only at the first actual root.
        for ancestor in start.ancestors() {
            let loader = ancestor.join(".pnp.cjs");
            paths.push(loader.clone());
            if loader.is_file() {
                paths.push(ancestor.join(".pnp.data.json"));
                paths.push(ancestor.join("yarn.lock"));
                if ancestor.join("package.json").is_file() {
                    paths.push(ancestor.join("package.json"));
                }
                break;
            }
            if ancestor == discovery_boundary {
                break;
            }
        }

        // Lockfiles are invalidation witnesses rather than resolver configuration. Retain only
        // the nearest discovered installation root, while watching its sibling lock names so a
        // package-manager switch cannot pass unnoticed.
        if let Some(install_root) = install_root {
            paths.extend(INSTALL_LOCKS.iter().map(|name| install_root.join(name)));
            let package = install_root.join("package.json");
            if package.is_file() {
                paths.push(package);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn project_control_interests_from(
    config_dir: &Path,
    root: &Path,
    explicit_entry: Option<&Path>,
) -> Vec<WatchInterest> {
    project_control_paths(config_dir, root, explicit_entry)
        .into_iter()
        .map(|path| WatchInterest::exact_file(path).resolve_against(root))
        .collect()
}

fn project_watch_interests(prepared: &PreparedBuild) -> Vec<WatchInterest> {
    project_watch_interests_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        prepared.explicit_entry.as_deref(),
        &prepared.config,
    )
}

fn probe_watch_interests(prepared: &PreparedBuildProbe) -> Vec<WatchInterest> {
    project_watch_interests_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        prepared.explicit_entry.as_deref(),
        &prepared.config,
    )
}

fn project_watch_interests_from(
    config_dir: &Path,
    root: &Path,
    logical_entry: &Path,
    explicit_entry: Option<&Path>,
    config: &wake_config::Config,
) -> Vec<WatchInterest> {
    let source = {
        let source = root.join("src");
        if source.is_dir() {
            source
        } else {
            root.to_path_buf()
        }
    };
    let source_interest = if source == root {
        WatchInterest::tree(source).excluding_tree(root.join(".wake"))
    } else {
        WatchInterest::tree(source)
    };
    let mut interests = vec![
        source_interest,
        WatchInterest::all_files_tree(root.join("public")),
        WatchInterest::exact_file(root.join("index.html")),
    ];
    // Physical generated entries are projected at stable `.wake` module identities and are
    // driven by their source facts. Only a user-owned source entry is itself a watch input.
    if !logical_entry.starts_with(root.join(".wake")) {
        interests.push(WatchInterest::exact_file(logical_entry.to_path_buf()));
    }
    interests.extend(project_control_interests_from(
        config_dir,
        root,
        explicit_entry,
    ));
    interests.extend(
        config
            .component_scan
            .iter()
            .map(|rule| WatchInterest::tree(root.join(&rule.cwd))),
    );
    // Only configured aliases are additional source ownership. Reserved `.wake` inputs were
    // rejected during probing, so no physical generated-output exception is needed here.
    interests.extend(
        config
            .alias
            .values()
            .map(|target| alias_watch_interest(absolute_from(root, Path::new(target)))),
    );
    interests.extend(
        config
            .federation
            .exposes
            .values()
            .map(|expose| alias_watch_interest(absolute_from(root, Path::new(&expose.entry)))),
    );
    interests = interests
        .into_iter()
        .map(|interest| interest.resolve_against(root))
        .collect();
    interests.sort();
    interests.dedup();
    interests
}

fn build_context_watch_interests(
    prepared: &PreparedBuild,
    options: &BuildOptions,
) -> Vec<WatchInterest> {
    let discovery = build_context_discovery_interests(options, &prepared.root);
    build_context_watch_interests_with_floor(prepared, options, &discovery)
}

fn build_context_watch_interests_with_floor(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    discovery: &[WatchInterest],
) -> Vec<WatchInterest> {
    let project = build_context_interests_with_output(
        project_watch_interests(prepared),
        &prepared.root,
        &prepared.outdir,
        options,
    );
    union_watch_interests(&project, discovery)
}

fn build_context_probe_watch_interests(
    prepared: &PreparedBuildProbe,
    options: &BuildOptions,
) -> Vec<WatchInterest> {
    let discovery = build_context_discovery_interests(options, &prepared.root);
    build_context_probe_watch_interests_with_floor(prepared, options, &discovery)
}

fn build_context_probe_watch_interests_with_floor(
    prepared: &PreparedBuildProbe,
    options: &BuildOptions,
    discovery: &[WatchInterest],
) -> Vec<WatchInterest> {
    let project = build_context_interests_with_output(
        probe_watch_interests(prepared),
        &prepared.root,
        &prepared.outdir,
        options,
    );
    union_watch_interests(&project, discovery)
}

fn build_context_interests_with_output(
    mut interests: Vec<WatchInterest>,
    root: &Path,
    outdir: &Path,
    options: &BuildOptions,
) -> Vec<WatchInterest> {
    if options.write {
        interests = interests
            .into_iter()
            .map(|interest| {
                interest
                    .excluding_tree(outdir.to_path_buf())
                    .resolve_against(root)
            })
            .collect();
        interests.sort();
        interests.dedup();
    }
    interests
}

fn extend_runtime_source_interests(
    prepared: &PreparedBuild,
    interests: &mut Vec<WatchInterest>,
    aliases: &[(String, PathBuf)],
) {
    let internal = prepared.root.join(".wake");
    interests.extend(
        aliases
            .iter()
            .map(|(_, path)| path)
            .filter(|path| !path.starts_with(&internal))
            .map(|path| alias_watch_interest(path.clone()).resolve_against(&prepared.root)),
    );
    interests.sort();
    interests.dedup();
}

fn changed_matches(changed: &[PathBuf], interests: &[WatchInterest]) -> bool {
    changed
        .iter()
        .any(|path| interests.iter().any(|interest| interest.matches(path)))
}

fn control_file_fingerprint(path: &Path) -> ControlFingerprint {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return ControlFingerprint::Unreadable(error.kind()),
    };
    let mut context = DigestContext::new(&SHA256);
    let mut len = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => return ControlFingerprint::Unreadable(error.kind()),
        };
        context.update(&buffer[..read]);
        len = len.saturating_add(read as u64);
    }
    let mut sha256 = [0_u8; 32];
    sha256.copy_from_slice(context.finish().as_ref());
    ControlFingerprint::Present { len, sha256 }
}

fn prepared_control_fingerprint(prepared: &PreparedBuild, path: &Path) -> ControlFingerprint {
    captured_control_fingerprint(&prepared.control_fingerprints, path)
}

fn captured_control_fingerprint(
    controls: &[ControlFileFingerprint],
    path: &Path,
) -> ControlFingerprint {
    controls
        .iter()
        .find_map(|(control, fingerprint)| (control == path).then(|| fingerprint.clone()))
        .expect("authoritative project controls are captured by the build probe")
}

fn control_snapshot_changed_error(config_dir: &Path) -> WakeError {
    WakeError::new(
        "WAKE_WATCH_SNAPSHOT_CHANGED",
        "project control files changed while preparing the build; retry from a fresh snapshot",
    )
    .at(&config_dir.join(wake_config::CONFIG_FILE))
}

fn stable_project_control_snapshot(
    config_before: &ControlFingerprint,
    config_dir: &Path,
    root: &Path,
    explicit_entry: Option<&Path>,
) -> Result<Vec<ControlFileFingerprint>, WakeError> {
    if control_file_fingerprint(&config_dir.join(wake_config::CONFIG_FILE)) != *config_before {
        return Err(control_snapshot_changed_error(config_dir));
    }
    let first = build_control_fingerprints(config_dir, root, explicit_entry);
    let second = build_control_fingerprints(config_dir, root, explicit_entry);
    if first != second {
        return Err(control_snapshot_changed_error(config_dir));
    }
    Ok(second)
}

fn federation_lock_changed(
    changed: &[PathBuf],
    rescan: bool,
    interest: &WatchInterest,
    path: &Path,
    accepted: &ControlFingerprint,
) -> bool {
    (rescan || changed_matches(changed, std::slice::from_ref(interest)))
        && control_file_fingerprint(path) != *accepted
}

fn wake_error_diagnostic(error: WakeError) -> Diagnostic {
    let WakeError {
        code,
        message,
        path,
        diagnostics,
    } = error;
    let mut diagnostic = Diagnostic::error(message).with_code(code);
    if let Some(path) = path {
        diagnostic = diagnostic.with_path(path);
    }
    for detail in diagnostics {
        diagnostic = diagnostic.with_note(format!(
            "{}{}: {}",
            detail
                .code
                .as_deref()
                .map(|code| format!("[{code}] "))
                .unwrap_or_default(),
            detail.path.as_deref().unwrap_or("build"),
            detail.message
        ));
    }
    diagnostic
}

fn create_bundler_options(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
) -> Result<BundlerBuildOptions, WakeError> {
    let federation = if prepared.config.federation.enabled {
        FederationBuildPlan {
            remotes: prepared
                .config
                .federation
                .remotes
                .keys()
                .map(|name| name.as_str().to_owned())
                .collect(),
            shared: prepared
                .config
                .federation
                .shared
                .iter()
                .map(|(share_key, shared)| {
                    (share_key.clone(), share_key.clone(), shared.scope.clone())
                })
                .collect(),
            entry_export: (!prepared.config.federation.shared.is_empty()).then(|| {
                FederationEntryExport::page_scoped(
                    prepared.config.federation.name.as_str(),
                    federation::HOST_EXPOSE,
                )
            }),
            ..FederationBuildPlan::default()
        }
    } else {
        FederationBuildPlan::default()
    };

    Ok(BundlerBuildOptions {
        project_root: Some(prepared.root.clone()),
        resolve: ResolveOptions {
            alias: prepared.aliases.clone(),
            ..ResolveOptions::default()
        },
        define: build_defines(&prepared.config, !project_defaults),
        extract_css: project_defaults,
        asset_inline_limit: 4096,
        public_path: prepared.config.public_path().to_owned(),
        minify: project_defaults,
        dead_module_elimination: project_defaults,
        source_map: options.source_map,
        css_in_js: true,
        tree_shaking: project_defaults,
        code_splitting: project_defaults,
        persistent_cache: persistent_cache_path(&prepared.root, options.cache, "cache.bin"),
        jsx: JsxOptions {
            development: false,
            import_source: prepared.config.react.jsx_import_source.clone(),
        },
        federation,
        target_env: resolve_target_env(&prepared.config, &prepared.root)?,
        ..BundlerBuildOptions::default()
    })
}

fn create_bundle_options(
    prepared: &PreparedBuild,
    options: &ResolvedBundleOptions,
) -> Result<BundlerBuildOptions, WakeError> {
    let target_env = match options.platform {
        BuildPlatform::Browser => resolve_target_env(&prepared.config, &prepared.root)?,
        BuildPlatform::Node => node_target_env(
            options
                .target
                .as_deref()
                .expect("Node bundle target is normalized"),
        )?,
    };
    let browser = options.platform == BuildPlatform::Browser;
    Ok(BundlerBuildOptions {
        project_root: Some(prepared.root.clone()),
        resolve: ResolveOptions {
            alias: prepared.aliases.clone(),
            ..ResolveOptions::default()
        },
        platform: options.platform,
        module_format: options.format,
        external_packages: options.external.clone(),
        define: build_defines(&prepared.config, browser),
        // 省略 outfile 时保持旧的内存 browser bundle 资源阈值；精确 outfile 是严格
        // 单文件，因此必须内联资源，不能在目标目录旁静默生成额外文件。
        asset_inline_limit: if browser && options.outfile.is_none() {
            4096
        } else {
            usize::MAX
        },
        public_path: if browser {
            prepared.config.public_path().to_owned()
        } else {
            BundlerBuildOptions::default().public_path
        },
        minify: options.minify,
        dead_module_elimination: options.minify,
        source_map: options.source_map,
        css_in_js: browser,
        tree_shaking: options.minify,
        content_hash: false,
        persistent_cache: persistent_cache_path(&prepared.root, options.cache, "bundle-cache.bin"),
        jsx: JsxOptions {
            development: false,
            import_source: prepared.config.react.jsx_import_source.clone(),
        },
        target_env,
        ..BundlerBuildOptions::default()
    })
}

fn persistent_cache_path(root: &Path, enabled: bool, file_name: &str) -> Option<PathBuf> {
    enabled.then(|| root.join(".wake").join(file_name))
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

struct ProjectBuildSession {
    generation: BuildGeneration,
    application: BuildSession,
    federation_inputs: federation::ProductionFederationInputs,
}

fn create_session(
    prepared: &mut PreparedBuild,
    options: &BuildOptions,
    project_defaults: bool,
) -> Result<ProjectBuildSession, WakeError> {
    let bundler_options = create_bundler_options(prepared, options, project_defaults)?;
    let federation_inputs = federation::render_production_inputs(prepared, options)?;
    prepared.install_product_inputs(federation_inputs.files())?;
    let generation = BuildGeneration::new(prepared.generation.file_system());
    let application = generation.retained_session(bundler_options);
    Ok(ProjectBuildSession {
        generation,
        application,
        federation_inputs,
    })
}

struct PreparedApplicationOutput {
    result: BuildResult,
    output: BuildOutput,
    federation: federation::FederationArtifacts,
    html: String,
}

fn prepare_application_output(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    output: BuildOutput,
    duration_ms: f64,
    federation: federation::FederationArtifacts,
    diagnostic_file_system: &dyn FileSystem,
) -> Result<PreparedApplicationOutput, WakeError> {
    let diagnostics = diagnostic_infos(&output.diagnostics, &prepared.root, diagnostic_file_system);
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
            kind: OutputFileKind::Chunk,
            bytes: chunk.code.len(),
        })
        .chain(output.assets.iter().map(|asset| OutputFile {
            path: asset.file_name.clone(),
            kind: if asset.is_css {
                OutputFileKind::Css
            } else {
                OutputFileKind::Asset
            },
            bytes: asset.bytes.len(),
        }))
        .collect::<Vec<_>>();
    files.extend(federation.output_files());
    let html = emit_html(
        &output,
        &prepared.config,
        federation.bootstrap_file.as_deref(),
    );
    if options.write {
        files.push(OutputFile {
            path: "index.html".to_string(),
            kind: OutputFileKind::Html,
            bytes: html.len(),
        });
    }

    Ok(PreparedApplicationOutput {
        result: BuildResult {
            success: true,
            module_count: output.module_count + federation.module_count,
            updated_module_count: output.updated_module_count + federation.updated_module_count,
            cached_module_count: output.cached_module_count + federation.cached_module_count,
            duration_ms,
            output_dir: None,
            code: (!options.write).then(|| output.bundle.clone()),
            files,
            diagnostics,
        },
        output,
        federation,
        html,
    })
}

fn write_application_output(
    prepared: &PreparedBuild,
    application: &PreparedApplicationOutput,
    stage: &Path,
) -> Result<(), WakeError> {
    write_build_output(&application.output, stage)?;
    application.federation.write_public_to(stage)?;
    let html_path = output_file_path(stage, "index.html")?;
    atomic_write(&html_path, application.html.as_bytes())?;
    // Hidden source maps are written before the public tree commits. If this step fails, the
    // previously published generation remains untouched.
    application
        .federation
        .write_hidden_source_maps_to(&prepared.root)
}

fn finish_output(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    output: BuildOutput,
    started: Instant,
    federation_generation: Option<federation::PreparedFederationGeneration>,
    generation: &mut BuildGeneration,
    cancellation: &CancellationToken,
) -> Result<BuildResult, WakeError> {
    cancellation.check()?;
    let federation = federation::build_artifacts(
        prepared,
        &output,
        federation_generation,
        generation,
        cancellation,
    )?;
    cancellation.check()?;
    let diagnostic_file_system = generation.file_system_view();
    let mut application = prepare_application_output(
        prepared,
        options,
        output,
        0.0,
        federation,
        diagnostic_file_system.as_ref(),
    )?;
    if options.write {
        let (_, target) = publish_staged_output(
            &prepared.root,
            &[prepared.entry.as_path()],
            &prepared.outdir,
            OutputProduct::Application,
            cancellation,
            |stage| write_application_output(prepared, &application, stage),
        )?;
        application.result.output_dir = Some(target.to_string_lossy().into_owned());
    }
    application.result.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(application.result)
}

fn finish_bundle(
    prepared: &PreparedBuild,
    options: &ResolvedBundleOptions,
    output: BuildOutput,
    duration_ms: f64,
    diagnostic_file_system: &dyn FileSystem,
    protected_inputs: &[PathBuf],
    cancellation: &CancellationToken,
) -> Result<BundleResult, WakeError> {
    let diagnostics = diagnostic_infos(&output.diagnostics, &prepared.root, diagnostic_file_system);
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
        let mut candidates = vec![ExactOutput::write(path, code.as_bytes())];
        if let (Some(map), Some(map_path)) = (&source_map, &source_map_file) {
            candidates.push(ExactOutput::write(map_path, map.as_bytes()));
        }
        cancellation.commit(|| publish_exact_outputs(&candidates, protected_inputs))?;
    }
    let mut files = vec![OutputFile {
        path: output_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| output.entry().file_name.clone()),
        kind: OutputFileKind::Chunk,
        bytes: code.len(),
    }];
    if output_file.is_none() {
        files.extend(output.assets.iter().map(|asset| OutputFile {
            path: asset.file_name.clone(),
            kind: if asset.is_css {
                OutputFileKind::Css
            } else {
                OutputFileKind::Asset
            },
            bytes: asset.bytes.len(),
        }));
    }
    if let Some(map) = &source_map {
        files.push(OutputFile {
            path: source_map_file
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{}.map", output.entry().file_name)),
            kind: OutputFileKind::SourceMap,
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

fn output_file_path(outdir: &Path, relative: impl AsRef<Path>) -> Result<PathBuf, WakeError> {
    let relative = relative.as_ref();
    if relative.as_os_str().is_empty()
        || relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            format!(
                "generated output path must be a non-empty relative path without traversal: {}",
                relative.display()
            ),
        ));
    }
    Ok(outdir.join(relative))
}

fn write_build_output(output: &BuildOutput, outdir: &Path) -> Result<(), WakeError> {
    std::fs::create_dir_all(outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(outdir))?;
    for chunk in &output.chunks {
        let path = output_file_path(outdir, &chunk.file_name)?;
        atomic_write(&path, chunk.code.as_bytes())?;
        if let Some(map) = &chunk.source_map {
            let map_path = output_file_path(outdir, format!("{}.map", chunk.file_name))?;
            atomic_write(&map_path, map.as_bytes())?;
        }
    }
    for asset in &output.assets {
        let path = output_file_path(outdir, &asset.file_name)?;
        atomic_write(&path, &asset.bytes)?;
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
    let path = output_file_path(outdir, "manifest.json")?;
    atomic_write(
        &path,
        &serde_json::to_vec_pretty(&manifest).expect("manifest serialization"),
    )?;
    Ok(())
}

fn emit_html(
    output: &BuildOutput,
    config: &wake_config::Config,
    federation_bootstrap: Option<&str>,
) -> String {
    let scripts = if federation_bootstrap.is_some() {
        Vec::new()
    } else {
        output
            .chunks
            .iter()
            .filter(|chunk| chunk.is_entry)
            .map(|chunk| chunk.file_name.clone())
            .collect::<Vec<_>>()
    };
    let styles = output.entry().styles.clone();
    let mut html = wake_html::generate(
        None,
        &wake_html::HtmlInputs {
            scripts: &scripts,
            styles: &styles,
            public_path: config.public_path(),
        },
    );
    if let Some(bootstrap) = federation_bootstrap {
        let src = if config.public_path().is_empty() {
            bootstrap.to_owned()
        } else if config.public_path().ends_with('/') {
            format!("{}{bootstrap}", config.public_path())
        } else {
            format!("{}/{bootstrap}", config.public_path())
        };
        let script = format!("<script type=\"module\" src=\"{src}\"></script>\n");
        if let Some(position) = html.find("</head>") {
            html.insert_str(position, &script);
        } else {
            html.push_str(&script);
        }
    }
    html
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
        // `import.meta.hot` is a capability boundary, not an ordinary user define. Wake emits
        // classic-script chunks and its browser protocol performs full-page Live Reload, so no
        // caller may synthesize a truthy module-HMR object through configuration.
        if key == "import.meta.hot" {
            continue;
        }
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

fn virtual_entry_target(root: &Path, config: &wake_config::Config) -> PathBuf {
    let target = config
        .html
        .entry
        .as_deref()
        .unwrap_or("src/entry.tsx")
        .replace('\\', "/");
    absolute_from(root, Path::new(&target))
}

fn virtual_entry_in(
    config: &wake_config::Config,
    generation: &mut GenerationDraft,
) -> Result<PathBuf, WakeError> {
    let target = config
        .html
        .entry
        .as_deref()
        .unwrap_or("src/entry.tsx")
        .replace('\\', "/");
    generation.write_file(
        Path::new("entry.tsx"),
        format!("import(\"@@/{target}\");\n").as_bytes(),
    )
}

fn metadata_is_link_or_reparse_point(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn resolve_physical_output_path_with_project_root(
    path: &Path,
    project_root: Option<&Path>,
) -> Result<PathBuf, WakeError> {
    if !path.is_absolute() {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            format!(
                "output path was not resolved to an absolute path: {}",
                path.display()
            ),
        )
        .at(path));
    }

    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata_is_link_or_reparse_point(&metadata) => {
                let is_project_ancestor_alias = project_root.is_some_and(|project_root| {
                    ancestor
                        .canonicalize()
                        .map(|physical| {
                            let physical = wake_common::fs::normalize(&physical);
                            physical != project_root
                                && project_root.starts_with(&physical)
                                && ancestor.components().count() < project_root.components().count()
                        })
                        .unwrap_or(false)
                });
                if !is_project_ancestor_alias {
                    return Err(WakeError::new(
                        "WAKE_CONFIG",
                        format!(
                            "refusing to publish output through a symbolic link or reparse point: {}",
                            ancestor.display()
                        ),
                    )
                    .at(ancestor));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(WakeError::new("WAKE_IO", error.to_string()).at(ancestor));
            }
        }
    }

    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    let existing = loop {
        match std::fs::symlink_metadata(&cursor) {
            Ok(_) => break cursor,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(WakeError::new(
                        "WAKE_CONFIG",
                        format!("refusing unsafe output directory: {}", path.display()),
                    )
                    .at(path));
                };
                missing.push(name.to_os_string());
                cursor = cursor
                    .parent()
                    .unwrap_or_else(|| Path::new(""))
                    .to_path_buf();
            }
            Err(error) => return Err(WakeError::new("WAKE_IO", error.to_string()).at(&cursor)),
        }
    };
    let mut physical = existing
        .canonicalize()
        .map(|path| wake_common::fs::normalize(&path))
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&existing))?;
    for component in missing.into_iter().rev() {
        physical.push(component);
    }
    Ok(normalize_path(&physical))
}

fn resolve_physical_output_path(path: &Path) -> Result<PathBuf, WakeError> {
    resolve_physical_output_path_with_project_root(path, None)
}

fn validate_output_ownership(target: &Path, product: OutputProduct) -> Result<(), WakeError> {
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(WakeError::new("WAKE_IO", error.to_string()).at(target)),
    };
    if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_dir() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "output target must be a real directory owned by Wake: {}",
                target.display()
            ),
        )
        .at(target));
    }
    let entries = std::fs::read_dir(target)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(target))?;
    let mut contains_output = false;
    for entry in entries {
        let entry =
            entry.map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(target))?;
        let path = entry.path();
        if is_output_commit_lock_path(&path) {
            let metadata = std::fs::symlink_metadata(&path)
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
            if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_file() {
                return Err(WakeError::new(
                    "WAKE_CONFIG",
                    "invalid Wake output-commit lock metadata",
                )
                .at(&path));
            }
        } else {
            contains_output = true;
        }
    }
    if !contains_output {
        return Ok(());
    }

    let marker = target.join(OUTPUT_OWNERSHIP_FILE);
    let marker_metadata = std::fs::symlink_metadata(&marker).map_err(|error| {
        WakeError::new(
            "WAKE_CONFIG",
            format!(
                "refusing to replace non-empty output without a valid {OUTPUT_OWNERSHIP_FILE}: {error}"
            ),
        )
        .at(target)
    })?;
    if metadata_is_link_or_reparse_point(&marker_metadata) || !marker_metadata.is_file() {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Wake output ownership marker: {}", marker.display()),
        )
        .at(&marker));
    }
    let bytes = std::fs::read(&marker)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&marker))?;
    let ownership = serde_json::from_slice::<OutputOwnership>(&bytes).map_err(|error| {
        WakeError::new(
            "WAKE_CONFIG",
            format!("invalid Wake output ownership marker: {error}"),
        )
        .at(&marker)
    })?;
    let expected = OutputOwnership::wake(product);
    if ownership != expected {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "output directory is owned by `{}` rather than `{}`",
                ownership.product,
                product.as_str()
            ),
        )
        .at(target));
    }
    Ok(())
}

fn resolve_safe_output_directory(
    project_root: &Path,
    protected_inputs: &[&Path],
    requested: &Path,
    product: OutputProduct,
) -> Result<PathBuf, WakeError> {
    let target = resolve_physical_output_path_with_project_root(requested, Some(project_root))?;
    if target.file_name().is_none()
        || target == project_root
        || project_root.starts_with(&target)
        || protected_inputs
            .iter()
            .any(|input| input.starts_with(&target))
    {
        return Err(WakeError::new(
            "WAKE_CONFIG",
            format!(
                "refusing to publish {} output over a project, project ancestor, or input: {}",
                product.as_str(),
                target.display()
            ),
        )
        .at(&target));
    }
    validate_output_ownership(&target, product)?;
    Ok(target)
}

fn write_output_ownership_marker(staging: &Path, product: OutputProduct) -> Result<(), WakeError> {
    let marker = staging.join(OUTPUT_OWNERSHIP_FILE);
    let bytes = serde_json::to_vec_pretty(&OutputOwnership::wake(product))
        .expect("output ownership serialization");
    atomic_write(&marker, &bytes)
}

fn publish_staged_output<T>(
    project_root: &Path,
    protected_inputs: &[&Path],
    requested: &Path,
    product: OutputProduct,
    cancellation: &CancellationToken,
    materialize: impl FnOnce(&Path) -> Result<T, WakeError>,
) -> Result<(T, PathBuf), WakeError> {
    let target = resolve_safe_output_directory(project_root, protected_inputs, requested, product)?;
    // Application and Docs staging belongs to the project domain, never the target's parent. A
    // valid output target cannot equal or contain `project_root`, so an ancestor output commit
    // cannot move or stale-clean a child publication's uncommitted staging tree.
    let staging = tempfile::Builder::new()
        .prefix(".wake-output-stage-")
        .tempdir_in(project_root)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(project_root))?;
    let stage_root = staging.path().join("output");
    std::fs::create_dir_all(&stage_root)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&stage_root))?;
    let value = materialize(&stage_root)?;
    write_output_ownership_marker(&stage_root, product)?;
    cancellation.commit(|| {
        commit_staged_output_with(
            &stage_root,
            &target,
            None,
            product.as_str(),
            product.backup_prefix(),
            || {
                let locked_target = resolve_safe_output_directory(
                    project_root,
                    protected_inputs,
                    requested,
                    product,
                )?;
                if locked_target != target {
                    return Err(WakeError::new(
                        "WAKE_OUTPUT_COLLISION",
                        "output target identity changed while waiting for its publication lock",
                    )
                    .at(&locked_target));
                }
                Ok(())
            },
            None,
        )
    })?;
    Ok((value, target))
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&root.join(path))
    }
}

fn absolute_from_project_root(
    configured_root: &Path,
    physical_root: &Path,
    path: &Path,
) -> PathBuf {
    if !path.is_absolute() {
        return normalize_path(&physical_root.join(path));
    }
    let path = normalize_path(path);
    path.strip_prefix(configured_root)
        .map(|relative| normalize_path(&physical_root.join(relative)))
        .unwrap_or(path)
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
        invalidation: WatchInvalidation,
        covered_revision: Option<WatchPlanRevision>,
        cancellation: CancellationToken,
        response: mpsc::Sender<Result<BuildResult, WakeError>>,
    },
    Close {
        response: mpsc::Sender<()>,
    },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchPlanRevision(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchPlanSnapshot {
    pub revision: WatchPlanRevision,
    pub root: PathBuf,
    pub interests: Vec<WatchInterest>,
}

/// Probe-only state for a watched build before it is allowed to create generated inputs or a
/// retained build session. A waiting bootstrap is recoverable: callers keep its plan installed,
/// report the diagnostic, and retry activation after a matching filesystem event or Rescan.
#[derive(Debug, Clone)]
pub enum BuildWatchBootstrapState {
    Waiting {
        plan: WatchPlanSnapshot,
        error: WakeError,
    },
    Activatable {
        plan: WatchPlanSnapshot,
    },
    Activated {
        plan: WatchPlanSnapshot,
    },
}

impl BuildWatchBootstrapState {
    pub fn plan(&self) -> &WatchPlanSnapshot {
        match self {
            Self::Waiting { plan, .. } | Self::Activatable { plan } | Self::Activated { plan } => {
                plan
            }
        }
    }
}

enum BuildWatchBootstrapCandidate {
    Waiting,
    Probe {
        identity: BuildProbeIdentity,
        probe: Box<PreparedBuildProbe>,
    },
    Prepared {
        identity: BuildProbeIdentity,
        prepared: Box<PreparedBuild>,
        interests: Vec<WatchInterest>,
    },
    Activated,
}

/// Application-owned startup transaction for `build --watch`.
///
/// Unlike [`BuildContext::create`], construction is strictly probe-only. The caller must install
/// the published plan and present its exact revision to [`Self::activate_at`]. Activation probes
/// again, materializes a candidate, publishes any refined coverage, and creates the retained
/// context only after that refined revision is also covered.
pub struct BuildWatchBootstrap {
    options: BuildOptions,
    fallback_root: PathBuf,
    plan: WatchPlanSnapshot,
    candidate: BuildWatchBootstrapCandidate,
    error: Option<WakeError>,
    #[cfg(test)]
    refined_interest_for_test: Option<WatchInterest>,
}

impl BuildWatchBootstrap {
    pub fn create(options: BuildOptions) -> Result<Self, WakeError> {
        let fallback_root = validate_build_watch_bootstrap_options(&options)?;
        match probe_build_candidate(&options) {
            Ok(probe) => {
                let plan = WatchPlanSnapshot {
                    revision: WatchPlanRevision(0),
                    root: probe.root.clone(),
                    interests: build_watch_bootstrap_probe_interests(
                        &options,
                        &fallback_root,
                        &probe,
                    ),
                };
                let identity = build_probe_identity(&probe);
                let mut bootstrap = Self {
                    options,
                    fallback_root,
                    plan,
                    candidate: BuildWatchBootstrapCandidate::Probe {
                        identity,
                        probe: Box::new(probe),
                    },
                    error: None,
                    #[cfg(test)]
                    refined_interest_for_test: None,
                };
                if let Err(error) = validate_build_watch_probe(match &bootstrap.candidate {
                    BuildWatchBootstrapCandidate::Probe { probe, .. } => probe,
                    _ => unreachable!("a successful constructor owns a probe"),
                }) {
                    bootstrap.set_waiting(error);
                }
                Ok(bootstrap)
            }
            Err(error) => {
                let interests = build_watch_bootstrap_recovery_interests(
                    &options,
                    &fallback_root,
                    &fallback_root,
                    &error,
                );
                Ok(Self {
                    options,
                    fallback_root: fallback_root.clone(),
                    plan: WatchPlanSnapshot {
                        revision: WatchPlanRevision(0),
                        root: fallback_root,
                        interests,
                    },
                    candidate: BuildWatchBootstrapCandidate::Waiting,
                    error: Some(error),
                    #[cfg(test)]
                    refined_interest_for_test: None,
                })
            }
        }
    }

    pub fn state(&self) -> BuildWatchBootstrapState {
        let plan = self.plan.clone();
        match (&self.candidate, &self.error) {
            (BuildWatchBootstrapCandidate::Waiting, Some(error)) => {
                BuildWatchBootstrapState::Waiting {
                    plan,
                    error: error.clone(),
                }
            }
            (BuildWatchBootstrapCandidate::Activated, _) => {
                BuildWatchBootstrapState::Activated { plan }
            }
            _ => BuildWatchBootstrapState::Activatable { plan },
        }
    }

    pub fn watch_plan(&self) -> WatchPlanSnapshot {
        self.plan.clone()
    }

    /// Activate only under a capability for the currently published bootstrap revision.
    /// `WAKE_WATCH_COVERAGE_PENDING` is an internal frontend control signal: callers install the
    /// newly published plan and retry with an authoritative Rescan without reporting it as a build
    /// diagnostic.
    pub fn activate_at(
        &mut self,
        covered_revision: WatchPlanRevision,
    ) -> Result<BuildContext, WakeError> {
        if matches!(self.candidate, BuildWatchBootstrapCandidate::Activated) {
            return Err(WakeError::new(
                "WAKE_INTERNAL",
                "build watch bootstrap was already activated",
            ));
        }
        if covered_revision != self.plan.revision {
            return Err(watch_coverage_pending_error());
        }

        // A previously prepared value was rendered only to discover refined interests. Once the
        // caller presents coverage for that revision, start from a fresh probe and render again;
        // source files may have changed while the watcher was installing the wider plan even when
        // the control-file identity stayed constant.
        self.reprobe(false)?;
        if covered_revision != self.plan.revision {
            return Err(watch_coverage_pending_error());
        }

        if matches!(self.candidate, BuildWatchBootstrapCandidate::Probe { .. }) {
            let (identity, probe) = match &self.candidate {
                BuildWatchBootstrapCandidate::Probe { identity, probe } => {
                    (identity.clone(), probe.as_ref().clone())
                }
                _ => unreachable!("bootstrap probe state was checked"),
            };
            let prepared = match materialize_build_probe(probe, true) {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.set_waiting(error.clone());
                    return Err(error);
                }
            };
            let refined = build_context_watch_interests(&prepared, &self.options);
            #[cfg(test)]
            let refined = {
                let mut refined = refined;
                if let Some(interest) = self.refined_interest_for_test.take() {
                    refined.push(interest.resolve_against(&prepared.root));
                }
                refined
            };
            self.merge_plan(prepared.root.clone(), refined.clone());
            self.candidate = BuildWatchBootstrapCandidate::Prepared {
                identity,
                prepared: Box::new(prepared),
                interests: refined,
            };
            if covered_revision != self.plan.revision {
                return Err(watch_coverage_pending_error());
            }

            // Materialization can overlap a control-file replacement. Never hand a prepared
            // generation to a retained context until a fresh probe proves it still represents the
            // same authoritative snapshot.
            let prepared_identity = match &self.candidate {
                BuildWatchBootstrapCandidate::Prepared { identity, .. } => identity.clone(),
                _ => unreachable!("candidate was just prepared"),
            };
            self.reprobe(true)?;
            let still_current = matches!(
                &self.candidate,
                BuildWatchBootstrapCandidate::Prepared { identity, .. }
                    if *identity == prepared_identity
            );
            if !still_current || covered_revision != self.plan.revision {
                return Err(watch_coverage_pending_error());
            }
        }

        let (prepared, interests) = match &self.candidate {
            BuildWatchBootstrapCandidate::Prepared {
                prepared,
                interests,
                ..
            } => (prepared.as_ref().clone(), interests.clone()),
            BuildWatchBootstrapCandidate::Waiting => {
                return Err(self.error.clone().unwrap_or_else(|| {
                    WakeError::new("WAKE_INTERNAL", "build watch bootstrap lost its diagnostic")
                }));
            }
            BuildWatchBootstrapCandidate::Activated => {
                return Err(WakeError::new(
                    "WAKE_INTERNAL",
                    "build watch bootstrap was already activated",
                ));
            }
            BuildWatchBootstrapCandidate::Probe { .. } => {
                return Err(watch_coverage_pending_error());
            }
        };
        let context =
            BuildContext::from_prepared_with_interests(self.options.clone(), prepared, interests)?;
        self.candidate = BuildWatchBootstrapCandidate::Activated;
        self.error = None;
        Ok(context)
    }

    fn reprobe(&mut self, retain_matching_prepared: bool) -> Result<(), WakeError> {
        let probe = match probe_build_candidate(&self.options) {
            Ok(probe) => probe,
            Err(error) => {
                self.set_waiting(error.clone());
                return Err(error);
            }
        };
        let identity = build_probe_identity(&probe);
        let interests =
            build_watch_bootstrap_probe_interests(&self.options, &self.fallback_root, &probe);
        self.merge_plan(probe.root.clone(), interests);

        if let Err(error) = validate_build_watch_probe(&probe) {
            self.set_waiting(error.clone());
            return Err(error);
        }

        let retain_prepared = retain_matching_prepared
            && matches!(
                &self.candidate,
                BuildWatchBootstrapCandidate::Prepared {
                    identity: current,
                    ..
                } if *current == identity
            );
        if !retain_prepared {
            self.candidate = BuildWatchBootstrapCandidate::Probe {
                identity,
                probe: Box::new(probe),
            };
        }
        self.error = None;
        Ok(())
    }

    fn set_waiting(&mut self, error: WakeError) {
        let watch_root = self.plan.root.clone();
        let recovery = build_watch_bootstrap_recovery_interests(
            &self.options,
            &self.fallback_root,
            &watch_root,
            &error,
        );
        self.merge_plan(self.plan.root.clone(), recovery);
        self.candidate = BuildWatchBootstrapCandidate::Waiting;
        self.error = Some(error);
    }

    fn merge_plan(&mut self, root: PathBuf, interests: Vec<WatchInterest>) {
        let interests = union_watch_interests(&self.plan.interests, &interests);
        if self.plan.root != root || self.plan.interests != interests {
            self.plan.revision.0 = self.plan.revision.0.saturating_add(1);
            self.plan.root = root;
            self.plan.interests = interests;
        }
    }
}

fn validate_build_watch_probe(probe: &PreparedBuildProbe) -> Result<(), WakeError> {
    if !probe.logical_entry.is_file() {
        return Err(WakeError::new(
            "WAKE_IO",
            format!(
                "entry file does not exist: {}",
                probe.logical_entry.display()
            ),
        )
        .at(&probe.logical_entry));
    }
    Ok(())
}

fn validate_build_watch_bootstrap_options(options: &BuildOptions) -> Result<PathBuf, WakeError> {
    let process_cwd = std::env::current_dir()
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()))?;
    let cwd = options
        .project
        .cwd
        .as_deref()
        .map(|cwd| absolute_from(&process_cwd, cwd))
        .unwrap_or(process_cwd);
    if !cwd.is_dir() {
        return Err(
            WakeError::new("WAKE_CONFIG", "cwd does not exist or is not a directory").at(&cwd),
        );
    }
    if let Some(config_path) = options.project.config_path.as_deref() {
        let path = absolute_from(&cwd, config_path);
        if path.file_name().and_then(|name| name.to_str()) != Some(wake_config::CONFIG_FILE) {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                format!("configPath must point to {}", wake_config::CONFIG_FILE),
            )
            .at(&path));
        }
    }
    if let Some(federation) = &options.federation {
        federation
            .clone()
            .validate_and_normalize()
            .map_err(|error| WakeError::new("FED_CONFIG_INVALID", error.to_string()).at(&cwd))?;
    }
    // Validate the physical directory without discarding the caller's lexical identity. Exact
    // discovery witnesses resolved from a symlinked cwd must retain both declared and canonical
    // paths so replacing or redirecting that symlink can invalidate the bootstrap.
    canonical_project_root(&cwd)?;
    Ok(cwd)
}

fn build_context_discovery_interests(
    options: &BuildOptions,
    project_root: &Path,
) -> Vec<WatchInterest> {
    let process_cwd = std::env::current_dir().unwrap_or_else(|_| project_root.to_path_buf());
    let fallback_root = options
        .project
        .cwd
        .as_deref()
        .map(|cwd| absolute_from(&process_cwd, cwd))
        .unwrap_or(process_cwd);
    build_watch_bootstrap_discovery_interests(options, &fallback_root)
}

fn build_watch_bootstrap_discovery_interests(
    options: &BuildOptions,
    fallback_root: &Path,
) -> Vec<WatchInterest> {
    let mut paths = Vec::new();
    if let Some(config_path) = options.project.config_path.as_deref() {
        paths.push(absolute_from(fallback_root, config_path));
    } else {
        for ancestor in fallback_root.ancestors() {
            let config = ancestor.join(wake_config::CONFIG_FILE);
            let package = ancestor.join("package.json");
            paths.push(config.clone());
            paths.push(package.clone());
            if config.is_file() || package.is_file() {
                break;
            }
        }
    }
    if let Some(entry) = options.entry.as_deref() {
        paths.push(absolute_from(fallback_root, entry));
    }
    let mut interests = paths
        .into_iter()
        .map(|path| WatchInterest::exact_file(path).resolve_against(fallback_root))
        .collect::<Vec<_>>();
    interests.sort();
    interests.dedup();
    interests
}

fn build_watch_bootstrap_probe_interests(
    options: &BuildOptions,
    fallback_root: &Path,
    probe: &PreparedBuildProbe,
) -> Vec<WatchInterest> {
    union_watch_interests(
        &build_context_probe_watch_interests(probe, options),
        &build_watch_bootstrap_discovery_interests(options, fallback_root),
    )
}

fn build_watch_bootstrap_recovery_interests(
    options: &BuildOptions,
    fallback_root: &Path,
    watch_root: &Path,
    error: &WakeError,
) -> Vec<WatchInterest> {
    let mut interests = build_watch_bootstrap_discovery_interests(options, fallback_root);
    if let Some(path) = error.path.as_deref() {
        let path = absolute_from(watch_root, Path::new(path));
        let generated = path.starts_with(watch_root.join(".wake"));
        let exact = generated
            || path.file_name().and_then(|name| name.to_str()) == Some(wake_config::CONFIG_FILE);
        if !exact
            || !interests
                .iter()
                .any(|interest| interest.matches_exact_file(&path))
        {
            let interest = if exact {
                WatchInterest::exact_file(path)
            } else {
                WatchInterest::tree(path)
            };
            interests.push(interest.resolve_against(watch_root));
        }
    }
    if options.write {
        let outdir = absolute_from(
            watch_root,
            options
                .outdir
                .as_deref()
                .unwrap_or_else(|| Path::new("dist")),
        );
        interests = interests
            .into_iter()
            .map(|interest| {
                interest
                    .excluding_tree(outdir.clone())
                    .resolve_against(watch_root)
            })
            .collect();
        interests.sort();
        interests.dedup();
    }
    interests
}

struct BuildContextInner {
    sender: mpsc::Sender<ContextCommand>,
    join: Mutex<Option<thread::JoinHandle<()>>>,
    closed: AtomicBool,
    watch_plan: Arc<RwLock<WatchPlanSnapshot>>,
}

#[derive(Clone)]
pub struct BuildContext {
    inner: Arc<BuildContextInner>,
}

impl BuildContext {
    pub fn create(options: BuildOptions) -> Result<Self, WakeError> {
        // A retained context owns an isolated generation. Stable `.wake` paths are reserved for
        // one-shot production so concurrent watch/dev processes cannot overwrite one another.
        let prepared = prepare_build_candidate(&options)?;
        Self::from_prepared(options, prepared)
    }

    fn from_prepared(options: BuildOptions, prepared: PreparedBuild) -> Result<Self, WakeError> {
        let interests = build_context_watch_interests(&prepared, &options);
        Self::from_prepared_with_interests(options, prepared, interests)
    }

    fn from_prepared_with_interests(
        options: BuildOptions,
        prepared: PreparedBuild,
        interests: Vec<WatchInterest>,
    ) -> Result<Self, WakeError> {
        let discovery_floor = build_context_discovery_interests(&options, &prepared.root);
        let interests = union_watch_interests(&interests, &discovery_floor);
        let watch_plan = Arc::new(RwLock::new(WatchPlanSnapshot {
            revision: WatchPlanRevision(0),
            root: prepared.root.clone(),
            interests,
        }));
        let worker_watch_plan = Arc::clone(&watch_plan);
        let (sender, receiver) = mpsc::channel();
        let join = thread::Builder::new()
            .name("wake-build-context".to_string())
            .spawn(move || {
                let mut prepared = prepared;
                let session = create_session(&mut prepared, &options, true);
                run_build_context(
                    receiver,
                    prepared,
                    options,
                    session,
                    worker_watch_plan,
                    discovery_floor,
                );
            })
            .map_err(|error| WakeError::new("WAKE_INTERNAL", error.to_string()))?;
        Ok(Self {
            inner: Arc::new(BuildContextInner {
                sender,
                join: Mutex::new(Some(join)),
                closed: AtomicBool::new(false),
                watch_plan,
            }),
        })
    }

    pub fn rebuild(
        &self,
        changed_paths: Vec<PathBuf>,
        cancellation: CancellationToken,
    ) -> Result<BuildResult, WakeError> {
        let invalidation = if changed_paths.is_empty() {
            WatchInvalidation::Rescan
        } else {
            WatchInvalidation::Paths(changed_paths)
        };
        self.send_rebuild(invalidation, cancellation, None)
    }

    pub fn rebuild_watch(
        &self,
        invalidation: WatchInvalidation,
        cancellation: CancellationToken,
    ) -> Result<BuildResult, WakeError> {
        self.send_rebuild(invalidation, cancellation, None)
    }

    /// Rebuild after a watcher frontend has confirmed complete coverage for `covered_revision`.
    /// If probing or materialization widens the plan, no generated input or build is allowed until
    /// the frontend installs the new revision and calls this method again with that capability.
    pub fn rebuild_watch_at(
        &self,
        invalidation: WatchInvalidation,
        covered_revision: WatchPlanRevision,
        cancellation: CancellationToken,
    ) -> Result<BuildResult, WakeError> {
        self.send_rebuild(invalidation, cancellation, Some(covered_revision))
    }

    fn send_rebuild(
        &self,
        invalidation: WatchInvalidation,
        cancellation: CancellationToken,
        covered_revision: Option<WatchPlanRevision>,
    ) -> Result<BuildResult, WakeError> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(WakeError::closed("BuildContext"));
        }
        let invalidation = invalidation.normalized();
        let (sender, receiver) = mpsc::channel();
        self.inner
            .sender
            .send(ContextCommand::Rebuild {
                invalidation,
                covered_revision,
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

    /// The authoritative typed watch plan for `build --watch` frontends. It can widen after a
    /// failed candidate build so that creating a newly configured alias target triggers recovery.
    pub fn watch_interests(&self) -> Vec<WatchInterest> {
        self.watch_plan().interests
    }

    /// One atomic view of the application-owned watch plan. Frontends install all interests for a
    /// revision before rebuilding and compare the post-build revision to close widening races.
    pub fn watch_plan(&self) -> WatchPlanSnapshot {
        self.inner
            .watch_plan
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

impl Drop for BuildContextInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let (sender, _) = mpsc::channel();
        let _ = self.sender.send(ContextCommand::Close { response: sender });
    }
}

fn replace_build_watch_plan(
    watch_plan: &RwLock<WatchPlanSnapshot>,
    root: PathBuf,
    mut interests: Vec<WatchInterest>,
) {
    interests.sort();
    interests.dedup();
    let mut plan = watch_plan
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if plan.root != root || plan.interests != interests {
        plan.revision.0 = plan.revision.0.saturating_add(1);
        plan.root = root;
        plan.interests = interests;
    }
}

fn union_build_watch_plan(
    watch_plan: &RwLock<WatchPlanSnapshot>,
    root: PathBuf,
    candidate: &[WatchInterest],
) {
    let current = watch_plan
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .interests
        .clone();
    replace_build_watch_plan(watch_plan, root, union_watch_interests(&current, candidate));
}

struct BuildContextCandidate {
    identity: BuildProbeIdentity,
    probe: PreparedBuildProbe,
    materialized: Option<(PreparedBuild, Option<ProjectBuildSession>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BuildProbeIdentity {
    config_dir: PathBuf,
    root: PathBuf,
    logical_entry: PathBuf,
    explicit_entry: Option<PathBuf>,
    outdir: PathBuf,
    controls: Vec<ControlFileFingerprint>,
}

fn build_control_fingerprints(
    config_dir: &Path,
    root: &Path,
    explicit_entry: Option<&Path>,
) -> Vec<ControlFileFingerprint> {
    project_control_paths(config_dir, root, explicit_entry)
        .into_iter()
        .map(|path| {
            let fingerprint = control_file_fingerprint(&path);
            (path, fingerprint)
        })
        .collect()
}

fn build_probe_identity(probe: &PreparedBuildProbe) -> BuildProbeIdentity {
    BuildProbeIdentity {
        config_dir: probe.config_dir.clone(),
        root: probe.root.clone(),
        logical_entry: probe.logical_entry.clone(),
        explicit_entry: probe.explicit_entry.clone(),
        outdir: probe.outdir.clone(),
        controls: probe.control_fingerprints.clone(),
    }
}

fn watch_coverage_pending_error() -> WakeError {
    WakeError::new(
        "WAKE_WATCH_COVERAGE_PENDING",
        "candidate watch coverage changed; install the latest watch-plan revision and rescan",
    )
}

fn run_build_context(
    receiver: mpsc::Receiver<ContextCommand>,
    prepared: PreparedBuild,
    options: BuildOptions,
    session: Result<ProjectBuildSession, WakeError>,
    watch_plan: Arc<RwLock<WatchPlanSnapshot>>,
    discovery_floor: Vec<WatchInterest>,
) {
    let (mut session, initial_error) = match session {
        Ok(session) => (Some(session), None),
        Err(error) => (None, Some(error)),
    };
    let stable_topology = build_topology(&prepared);
    let stable_federation_lock_fingerprint =
        prepared_control_fingerprint(&prepared, &prepared.root.join("wake-federation.lock"));
    let mut accepted_identity = build_probe_identity(&build_probe_from_prepared(&prepared));
    let mut refresh_state: RefreshState<PreparedBuild, BuildContextCandidate, WakeError> =
        RefreshState {
            accepted: prepared,
            next_id: 1,
            candidate: initial_error.map_or(CandidateState::Stable, |diagnostic| {
                CandidateState::Blocked { diagnostic }
            }),
        };
    while let Ok(command) = receiver.recv() {
        match command {
            ContextCommand::Rebuild {
                invalidation,
                covered_revision,
                cancellation,
                response,
            } => {
                let result = cancellation.check().and_then(|()| {
                    if covered_revision.is_some_and(|covered| {
                        covered
                            != watch_plan
                                .read()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .revision
                    }) {
                        return Err(watch_coverage_pending_error());
                    }
                    let rescan = invalidation.is_rescan();
                    let paths = invalidation
                        .paths()
                        .iter()
                        .map(|path| absolute_from(&refresh_state.accepted.root, path))
                        .collect::<Vec<_>>();
                    let control_interests = union_watch_interests(
                        &project_control_interests(&refresh_state.accepted),
                        &discovery_floor,
                    );
                    let federation_lock = WatchInterest::exact_file(
                        refresh_state.accepted.root.join("wake-federation.lock"),
                    )
                    .resolve_against(&refresh_state.accepted.root);
                    let federation_lock_changed = federation_lock_changed(
                        &paths,
                        rescan,
                        &federation_lock,
                        &refresh_state.accepted.root.join("wake-federation.lock"),
                        &stable_federation_lock_fingerprint,
                    );
                    if federation_lock_changed {
                        let error = restart_required_error("Federation lock changed");
                        refresh_state.candidate = CandidateState::Blocked {
                            diagnostic: error.clone(),
                        };
                        return Err(error);
                    }
                    let control_changed = rescan || changed_matches(&paths, &control_interests);
                    if control_changed {
                        let probe = match probe_build_candidate(&options) {
                            Ok(probe) => probe,
                            Err(error) => {
                                let recovery = recovery_watch_interests(
                                    build_context_watch_interests_with_floor(
                                        &refresh_state.accepted,
                                        &options,
                                        &discovery_floor,
                                    ),
                                    &refresh_state.accepted.root,
                                    &error,
                                );
                                union_build_watch_plan(
                                    &watch_plan,
                                    refresh_state.accepted.root.clone(),
                                    &recovery,
                                );
                                refresh_state.candidate = CandidateState::Blocked {
                                    diagnostic: error.clone(),
                                };
                                return Err(error);
                            }
                        };
                        if let Some(reason) =
                            build_topology_change(&stable_topology, &build_probe_topology(&probe))
                        {
                            let error = restart_required_error(reason);
                            union_build_watch_plan(
                                &watch_plan,
                                refresh_state.accepted.root.clone(),
                                &build_context_probe_watch_interests_with_floor(
                                    &probe,
                                    &options,
                                    &discovery_floor,
                                ),
                            );
                            refresh_state.candidate = CandidateState::Blocked {
                                diagnostic: error.clone(),
                            };
                            return Err(error);
                        }
                        let identity = build_probe_identity(&probe);
                        if rescan
                            && matches!(&refresh_state.candidate, CandidateState::Stable)
                            && refresh_state.accepted.config.component_scan.is_empty()
                            && identity == accepted_identity
                            && let Some(accepted_session) = session.as_mut()
                        {
                            return execute_context_session_build(
                                &refresh_state.accepted,
                                &options,
                                accepted_session,
                                &[],
                                true,
                                &cancellation,
                            );
                        }
                        let same_pending = matches!(
                            &refresh_state.candidate,
                            CandidateState::Pending { draft, .. }
                                if draft.identity == identity
                        );
                        if !same_pending {
                            let preliminary = build_context_probe_watch_interests_with_floor(
                                &probe,
                                &options,
                                &discovery_floor,
                            );
                            union_build_watch_plan(
                                &watch_plan,
                                refresh_state.accepted.root.clone(),
                                &preliminary,
                            );
                            let id = refresh_state.next_id;
                            refresh_state.next_id = refresh_state.next_id.wrapping_add(1).max(1);
                            refresh_state.candidate = CandidateState::Pending {
                                id,
                                draft: BuildContextCandidate {
                                    identity,
                                    probe,
                                    materialized: None,
                                },
                                last_error: None,
                            };
                            let after = watch_plan
                                .read()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .revision;
                            if covered_revision.is_some_and(|covered| covered != after) {
                                return Err(watch_coverage_pending_error());
                            }
                        }
                    } else if let CandidateState::Blocked { diagnostic } = &refresh_state.candidate
                    {
                        return Err(diagnostic.clone());
                    } else if matches!(&refresh_state.candidate, CandidateState::Stable)
                        && refresh_state.accepted.config.component_scan.is_empty()
                    {
                        let mut invalidated = paths;
                        invalidated.sort();
                        invalidated.dedup();
                        return execute_context_session_build(
                            &refresh_state.accepted,
                            &options,
                            session.as_mut().expect("accepted build session"),
                            &invalidated,
                            true,
                            &cancellation,
                        );
                    } else if matches!(&refresh_state.candidate, CandidateState::Stable) {
                        let probe = build_probe_from_prepared(&refresh_state.accepted);
                        let id = refresh_state.next_id;
                        refresh_state.next_id = refresh_state.next_id.wrapping_add(1).max(1);
                        refresh_state.candidate = CandidateState::Pending {
                            id,
                            draft: BuildContextCandidate {
                                identity: build_probe_identity(&probe),
                                probe,
                                materialized: None,
                            },
                            last_error: None,
                        };
                    }

                    if matches!(&refresh_state.candidate, CandidateState::Pending { .. })
                        && covered_revision.is_some_and(|covered| {
                            covered
                                != watch_plan
                                    .read()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                                    .revision
                        })
                    {
                        return Err(watch_coverage_pending_error());
                    }

                    let pending =
                        std::mem::replace(&mut refresh_state.candidate, CandidateState::Stable);
                    let CandidateState::Pending {
                        id,
                        mut draft,
                        mut last_error,
                    } = pending
                    else {
                        return Err(WakeError::new(
                            "WAKE_INTERNAL",
                            "build refresh candidate state was lost",
                        ));
                    };

                    if draft.materialized.is_none() {
                        let candidate = match materialize_build_probe(draft.probe.clone(), true) {
                            Ok(candidate) => candidate,
                            Err(error) => {
                                last_error = Some(error.clone());
                                refresh_state.candidate = CandidateState::Pending {
                                    id,
                                    draft,
                                    last_error,
                                };
                                return Err(error);
                            }
                        };
                        // This first materialization exists only to discover the complete watch
                        // surface. Creating a retained bundler session here would let it escape
                        // before the frontend proves coverage for the refined plan.
                        draft.materialized = Some((candidate, None));
                    }

                    let refined = {
                        let (candidate, _) = draft
                            .materialized
                            .as_ref()
                            .expect("candidate was materialized");
                        build_context_watch_interests_with_floor(
                            candidate,
                            &options,
                            &discovery_floor,
                        )
                    };
                    union_build_watch_plan(
                        &watch_plan,
                        refresh_state.accepted.root.clone(),
                        &refined,
                    );
                    let after = watch_plan
                        .read()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .revision;
                    if covered_revision.is_some_and(|covered| covered != after) {
                        refresh_state.candidate = CandidateState::Pending {
                            id,
                            draft,
                            last_error,
                        };
                        return Err(watch_coverage_pending_error());
                    }

                    let (mut candidate, candidate_session) = draft
                        .materialized
                        .take()
                        .expect("candidate was materialized");
                    let candidate_identity = draft.identity.clone();
                    let generation_changed = match refresh_application_generation(&mut candidate) {
                        Ok(changed) => changed,
                        Err(error) => {
                            last_error = Some(error.clone());
                            draft.materialized = Some((candidate, candidate_session));
                            refresh_state.candidate = CandidateState::Pending {
                                id,
                                draft,
                                last_error,
                            };
                            return Err(error);
                        }
                    };
                    let candidate_session =
                        (!generation_changed).then_some(candidate_session).flatten();
                    let reused_session = candidate_session.is_some();
                    let mut candidate_session = match candidate_session {
                        Some(session) => session,
                        None => match create_session(&mut candidate, &options, true) {
                            Ok(session) => session,
                            Err(error) => {
                                last_error = Some(error.clone());
                                draft.materialized = Some((candidate, None));
                                refresh_state.candidate = CandidateState::Pending {
                                    id,
                                    draft,
                                    last_error,
                                };
                                return Err(error);
                            }
                        },
                    };
                    let mut invalidated = paths;
                    invalidated.extend(generation_changed_paths(
                        &refresh_state.accepted.generation,
                        &candidate.generation,
                    ));
                    invalidated.sort();
                    invalidated.dedup();
                    let result = execute_context_session_build(
                        &candidate,
                        &options,
                        &mut candidate_session,
                        &invalidated,
                        reused_session,
                        &cancellation,
                    );
                    if result.is_ok() {
                        let candidate_interests = build_context_watch_interests_with_floor(
                            &candidate,
                            &options,
                            &discovery_floor,
                        );
                        refresh_state.accepted = candidate;
                        accepted_identity = candidate_identity;
                        session = Some(candidate_session);
                        replace_build_watch_plan(
                            &watch_plan,
                            refresh_state.accepted.root.clone(),
                            candidate_interests,
                        );
                        refresh_state.candidate = CandidateState::Stable;
                    } else {
                        last_error = result.as_ref().err().cloned();
                        draft.materialized = Some((candidate, Some(candidate_session)));
                        refresh_state.candidate = CandidateState::Pending {
                            id,
                            draft,
                            last_error,
                        };
                    }
                    result
                });
                let _ = response.send(result);
            }
            ContextCommand::Close { response } => {
                let _ = response.send(());
                break;
            }
        }
    }
}

fn execute_context_session_build(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    session: &mut ProjectBuildSession,
    invalidated: &[PathBuf],
    invalidate: bool,
    cancellation: &CancellationToken,
) -> Result<BuildResult, WakeError> {
    let started = Instant::now();
    session.generation.advance_generation();
    let federation_generation = federation::bind_production_generation(
        prepared,
        options,
        &session.federation_inputs,
        session.generation.file_system_view(),
    )?;
    cancellation.check()?;
    if invalidate {
        if invalidated.is_empty() {
            session.application.invalidate_filesystem();
        } else {
            session.application.invalidate_paths(invalidated, true);
        }
    }
    let output = session
        .application
        .build_current(BuildRequest::new(&prepared.entry));
    cancellation.check()?;
    finish_output(
        prepared,
        options,
        output,
        started,
        federation_generation,
        &mut session.generation,
        cancellation,
    )
}
#[derive(Debug, Clone, Default)]
pub struct DevServerOptions {
    pub project: ProjectOptions,
    pub entry: Option<PathBuf>,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub open: Option<bool>,
    /// Programmatic federation override. `None` uses `wake.config.toml`.
    pub federation: Option<FederationOptions>,
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
    FederationUpdated {
        remote: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        old_build_id: Option<String>,
        new_build_id: String,
        changed_exposes: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        types_hash: Option<String>,
        action: wake_dev_server::DevUpdateAction,
    },
    Closed,
}

#[derive(Clone)]
pub struct DevServer {
    handle: wake_dev_server::ServerHandle,
    events: Arc<Mutex<mpsc::Receiver<DevServerEvent>>>,
    federation_type_monitor: Option<federation_type_watch::FederationTypeMonitor>,
}

impl DevServer {
    pub fn url(&self) -> &str {
        self.handle.url()
    }

    pub fn request_close(&self) {
        if let Some(monitor) = &self.federation_type_monitor {
            monitor.request_stop();
        }
        self.handle.request_close();
    }

    pub fn close(&self) -> Result<(), WakeError> {
        if let Some(monitor) = &self.federation_type_monitor {
            monitor.request_stop();
        }
        let result = self
            .handle
            .close()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()));
        if let Some(monitor) = &self.federation_type_monitor {
            monitor.stop_and_join();
        }
        result
    }

    pub fn wait_until_closed(&self) -> Result<(), WakeError> {
        let result = self
            .handle
            .wait()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()));
        if let Some(monitor) = &self.federation_type_monitor {
            monitor.stop_and_join();
        }
        result
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
        wake_dev_server::ServerEvent::Diagnostics {
            diagnostics,
            sources,
        } => {
            for diagnostic in diagnostic_infos_from_captured_sources(&diagnostics, sources) {
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
        wake_dev_server::ServerEvent::FederationUpdated {
            remote,
            old_build_id,
            new_build_id,
            changed_exposes,
            types_hash,
            action,
        } => {
            let _ = sender.send(DevServerEvent::FederationUpdated {
                remote,
                old_build_id,
                new_build_id,
                changed_exposes,
                types_hash,
                action,
            });
        }
        wake_dev_server::ServerEvent::Closed => {
            let _ = sender.send(DevServerEvent::Closed);
        }
    }
}

#[allow(clippy::result_large_err)]
pub fn start_dev_server(options: DevServerOptions) -> Result<DevServer, WakeError> {
    let dev_options = options.clone();
    let build_options = BuildOptions {
        project: options.project.clone(),
        entry: options.entry.clone(),
        write: false,
        federation: options.federation.clone(),
        ..BuildOptions::default()
    };
    let mut prepared = prepare_build_candidate(&build_options)?;
    let development_type_lock = if prepared.config.federation.enabled
        && prepared
            .config
            .federation
            .remotes
            .values()
            .any(|remote| !remote.dev_follow)
    {
        federation::load_production_lock(&prepared)?.map(Arc::new)
    } else {
        None
    };
    let initial_type_sync =
        if prepared.config.federation.enabled && !prepared.config.federation.remotes.is_empty() {
            // Development must never pair a newly followed remote build with stale declarations.
            // The synchronizer validates every remote first and only then atomically swaps the stable
            // editor index, so a partial network failure cannot publish a mixed build set.
            Some(federation_type_sync::sync_federation_types_for_development(
                &prepared.root,
                &prepared.config.federation,
                development_type_lock.as_deref(),
            )?)
        } else {
            None
        };
    let dev_federation = federation::prepare_dev(
        &prepared,
        &build_options,
        prepared.generation.file_system(),
        development_type_lock.clone(),
    )?;
    prepared.install_product_inputs(&dev_federation.generated_inputs)?;
    let mut dev_aliases = prepared.aliases.clone();
    dev_aliases.extend(dev_federation.aliases.clone());
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
    let initial_target_env = resolve_target_env(config, &prepared.root)?;
    let initial_topology = dev_topology(&prepared, &dev_options, 5173, &initial_target_env);
    let mut initial_watch_interests = project_watch_interests(&prepared);
    extend_runtime_source_interests(
        &prepared,
        &mut initial_watch_interests,
        &dev_federation.aliases,
    );
    let control_interests = project_control_interests(&prepared);
    let federation_lock_interest =
        WatchInterest::exact_file(prepared.root.join("wake-federation.lock"))
            .resolve_against(&prepared.root);
    let initial_federation_lock_fingerprint =
        prepared_control_fingerprint(&prepared, &prepared.root.join("wake-federation.lock"));
    let refresh_state: Arc<Mutex<RefreshState<PreparedBuild, PreparedBuildProbe>>> =
        Arc::new(Mutex::new(RefreshState {
            accepted: prepared.clone(),
            next_id: 1,
            candidate: CandidateState::Stable,
        }));
    let refresh_build_options = build_options.clone();
    let refresh_dev_options = dev_options.clone();
    let refresh: wake_dev_server::RefreshMount = Arc::new(move |_current, invalidation| {
        let changed = invalidation.paths();
        let rescan = invalidation.is_rescan();
        let accepted_root = refresh_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .accepted
            .root
            .clone();
        let current_controls = control_interests
            .iter()
            .map(|interest| interest.resolve_against(&accepted_root))
            .collect::<Vec<_>>();
        let current_federation_lock = federation_lock_interest.resolve_against(&accepted_root);
        let federation_lock_changed = federation_lock_changed(
            changed,
            rescan,
            &current_federation_lock,
            &accepted_root.join("wake-federation.lock"),
            &initial_federation_lock_fingerprint,
        );
        if federation_lock_changed {
            let diagnostic =
                wake_error_diagnostic(restart_required_error("Federation lock changed"));
            let mut state = refresh_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.candidate = CandidateState::Blocked { diagnostic };
            return Ok(wake_dev_server::DevMountRefresh::RestartRequired {
                reason: "Federation lock changed".to_owned(),
            });
        }
        let control_changed = rescan || changed_matches(changed, &current_controls);
        if !control_changed {
            let state = refresh_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match &state.candidate {
                CandidateState::Blocked { diagnostic } => {
                    return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                        watch_interests: project_watch_interests(&state.accepted),
                        diagnostic: diagnostic.clone(),
                    });
                }
                CandidateState::Pending { id, draft, .. } => {
                    let id = *id;
                    let draft = draft.clone();
                    drop(state);
                    return Ok(wake_dev_server::DevMountRefresh::Candidate(
                        make_dev_refresh_candidate(
                            Arc::clone(&refresh_state),
                            id,
                            draft,
                            refresh_build_options.clone(),
                        ),
                    ));
                }
                CandidateState::Stable => {}
            }
            let accepted = state.accepted.clone();
            drop(state);
            if accepted.config.component_scan.is_empty() {
                return Ok(wake_dev_server::DevMountRefresh::Invalidate {
                    generated_paths: Vec::new(),
                });
            }
            let draft = build_probe_from_prepared(&accepted);
            let id = {
                let mut state = refresh_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1).max(1);
                state.candidate = CandidateState::Pending {
                    id,
                    draft: draft.clone(),
                    last_error: None,
                };
                id
            };
            return Ok(wake_dev_server::DevMountRefresh::Candidate(
                make_dev_refresh_candidate(
                    Arc::clone(&refresh_state),
                    id,
                    draft,
                    refresh_build_options.clone(),
                ),
            ));
        }
        let refreshed = match probe_build_candidate(&refresh_build_options) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                let mut state = refresh_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let watch_interests = recovery_watch_interests(
                    project_watch_interests(&state.accepted),
                    &state.accepted.root,
                    &error,
                );
                let diagnostic = wake_error_diagnostic(error);
                state.candidate = CandidateState::Blocked {
                    diagnostic: diagnostic.clone(),
                };
                return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                });
            }
        };
        let refreshed_target = match resolve_target_env(&refreshed.config, &refreshed.root) {
            Ok(target) => target,
            Err(error) => {
                let diagnostic = wake_error_diagnostic(error);
                let mut state = refresh_state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let watch_interests = probe_watch_interests(&refreshed);
                state.candidate = CandidateState::Blocked {
                    diagnostic: diagnostic.clone(),
                };
                return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                });
            }
        };
        let refreshed_topology =
            dev_probe_topology(&refreshed, &refresh_dev_options, 5173, &refreshed_target);
        if let Some(reason) = dev_topology_change(&initial_topology, &refreshed_topology) {
            let diagnostic = wake_error_diagnostic(restart_required_error(reason));
            let watch_interests = probe_watch_interests(&refreshed);
            refresh_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .candidate = CandidateState::Blocked {
                diagnostic: diagnostic.clone(),
            };
            return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                watch_interests,
                diagnostic,
            });
        }
        let id = {
            let mut state = refresh_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1).max(1);
            state.candidate = CandidateState::Pending {
                id,
                draft: refreshed.clone(),
                last_error: None,
            };
            id
        };
        Ok(wake_dev_server::DevMountRefresh::Candidate(
            make_dev_refresh_candidate(
                Arc::clone(&refresh_state),
                id,
                refreshed,
                refresh_build_options.clone(),
            ),
        ))
    });
    let (event_tx, event_rx) = mpsc::channel();
    let federation_type_event_tx = event_tx.clone();
    let federation_type_monitor_slot: Arc<
        Mutex<Option<federation_type_watch::FederationTypeMonitor>>,
    > = Arc::new(Mutex::new(None));
    let event_monitor_slot = Arc::clone(&federation_type_monitor_slot);
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        if matches!(&event, wake_dev_server::ServerEvent::Closed)
            && let Some(monitor) = event_monitor_slot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
        {
            monitor.request_stop();
        }
        forward_dev_server_event(&event_tx, event);
    });
    let serve_options = wake_dev_server::ServeOptions {
        entry: dev_federation.entry,
        base_path: config.public_path().to_string(),
        resolve_options: ResolveOptions {
            alias: dev_aliases,
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
        target_env: initial_target_env,
        jsx_import_source: config.react.jsx_import_source.clone(),
        file_system: Some(prepared.generation.file_system()),
        watch_interests: initial_watch_interests,
        refresh: Some(refresh),
        quiet: true,
        event_handler: Some(event_handler),
        mounts: Vec::new(),
        deferred_mounts: Vec::new(),
        federation: dev_federation.build,
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    let federation_type_monitor = if let Some(initial_type_sync) = &initial_type_sync {
        let report_error = Arc::new(move |error: WakeError| {
            let WakeError {
                code,
                message,
                path,
                diagnostics: _,
            } = error;
            let mut diagnostic = Diagnostic::error(format!(
                "Federation type refresh failed; keeping the previous editor declarations: {message}"
            ))
            .with_code(code)
            .with_note("Wake will retry while the development server remains open");
            if let Some(path) = path {
                diagnostic = diagnostic.with_path(path);
            }
            let _ = federation_type_event_tx.send(DevServerEvent::Diagnostic {
                diagnostic: DiagnosticInfo::from_diagnostic(&diagnostic, None),
            });
        });
        match federation_type_watch::start_federation_type_monitor(
            &prepared.root,
            &prepared.config.federation,
            development_type_lock.as_deref(),
            initial_type_sync,
            report_error,
        ) {
            Ok(monitor) => monitor,
            Err(error) => {
                let _ = handle.close();
                return Err(error);
            }
        }
    } else {
        None
    };
    *federation_type_monitor_slot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = federation_type_monitor.clone();
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
        federation_type_monitor,
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
    let prepared_docs = prepare_docs(&options, wake_docs::BuildMode::Production, docs_mode)?;
    let requested = absolute_from(
        &prepared_docs.0.root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("docs-dist")),
    );
    let project_root = prepared_docs.0.root.clone();
    let protected_entry = prepared_docs.0.entry.clone();
    let (mut result, target) = publish_staged_output(
        &project_root,
        &[protected_entry.as_path()],
        &requested,
        OutputProduct::Documentation,
        cancellation,
        |stage| materialize_docs_leaf(prepared_docs, options, docs_mode, cancellation, stage),
    )?;
    result.build.output_dir = Some(target.to_string_lossy().into_owned());
    Ok(result)
}

fn build_docs_leaf_into(
    options: DocsBuildOptions,
    docs_mode: DocsMode,
    cancellation: &CancellationToken,
    outdir: &Path,
) -> Result<DocsBuildResult, WakeError> {
    let prepared_docs = prepare_docs(&options, wake_docs::BuildMode::Production, docs_mode)?;
    materialize_docs_leaf(prepared_docs, options, docs_mode, cancellation, outdir)
}

fn materialize_docs_leaf(
    prepared_docs: PreparedDocs,
    options: DocsBuildOptions,
    docs_mode: DocsMode,
    cancellation: &CancellationToken,
    outdir: &Path,
) -> Result<DocsBuildResult, WakeError> {
    cancellation.check()?;
    let started = Instant::now();
    let (mut prepared, docs, routes, demos, warnings, _changed_files) = prepared_docs;
    prepared.outdir = outdir.to_path_buf();
    prepared.config.public_path = Some(normalize_public_path(&docs.base_path));
    let build_options = BuildOptions {
        project: options.project,
        entry: Some(prepared.entry.clone()),
        outdir: Some(prepared.outdir.clone()),
        write: true,
        ..BuildOptions::default()
    };
    let mut bundler_options = create_bundler_options(&prepared, &build_options, true)?;
    bundler_options.entry_chunk_name = Some("entry".to_owned());
    let request = BuildRequest::new(&prepared.entry);
    let federation_inputs = federation::render_production_inputs(&prepared, &build_options)?;
    prepared.install_product_inputs(federation_inputs.files())?;
    let mut generation = BuildGeneration::new(prepared.generation.file_system());
    let federation_generation = federation::bind_production_generation(
        &prepared,
        &build_options,
        &federation_inputs,
        generation.file_system_view(),
    )?;
    cancellation.check()?;
    let output = generation.build_once(bundler_options, request);
    cancellation.check()?;
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", "Wake documentation build failed").with_diagnostic_infos(
                diagnostic_infos(
                    &output.diagnostics,
                    &prepared.root,
                    generation.file_system_view().as_ref(),
                ),
            ),
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
    let federation = federation::build_artifacts(
        &prepared,
        &output,
        federation_generation,
        &mut generation,
        cancellation,
    )?;
    let mut application = prepare_application_output(
        &prepared,
        &build_options,
        output,
        started.elapsed().as_secs_f64() * 1000.0,
        federation,
        generation.file_system_view().as_ref(),
    )?;
    write_application_output(&prepared, &application, outdir)?;
    application
        .result
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
        outdir,
        &routes,
        &html,
        &docs.title,
        &docs.description,
        &docs.locale,
    )
    .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    wake_docs::copy_public_assets(&prepared.root, outdir)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    application.result.files = docs_output_inventory(outdir)?;
    application.result.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(DocsBuildResult {
        build: application.result,
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
    let configured_root = normalize_path(&config.resolved_root(&config_dir));
    let site_root = canonical_project_root(&configured_root)?;
    let site_base = normalize_public_path(
        options
            .base_path
            .as_deref()
            .unwrap_or(&config.docs.base_path),
    );
    let requested_outdir = absolute_from_project_root(
        &configured_root,
        &site_root,
        options
            .outdir
            .as_deref()
            .unwrap_or_else(|| Path::new("docs-dist")),
    );
    let (mut result, final_outdir) = publish_staged_output(
        &site_root,
        &[],
        &requested_outdir,
        OutputProduct::Documentation,
        cancellation,
        |stage_root| {
            let mut site_options = options.clone();
            site_options.outdir = Some(stage_root.to_path_buf());
            site_options.presentation = Some(DocsPresentation::Standalone);
            let mut result =
                build_docs_leaf_into(site_options, DocsMode::Site, cancellation, stage_root)?;
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
                    wake_config::DocsWorkspacePresentation::Standalone => {
                        DocsPresentation::Standalone
                    }
                };
                let workspace_options = DocsBuildOptions {
                    project: ProjectOptions {
                        cwd: Some(workspace.config_dir.clone()),
                        config_path: None,
                    },
                    outdir: Some(workspace_outdir.clone()),
                    base_path: Some(workspace.base_path.clone()),
                    presentation: Some(presentation),
                };
                let mut workspace_result = build_docs_leaf_into(
                    workspace_options,
                    DocsMode::Components,
                    cancellation,
                    &workspace_outdir,
                )
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

            write_aggregate_docs_manifest(stage_root, &site_base, &workspace_infos)?;
            result.build.files = docs_output_inventory(stage_root)?;
            result.workspaces = workspace_infos;
            Ok(result)
        },
    )?;
    result.build.output_dir = Some(final_outdir.to_string_lossy().into_owned());
    result.build.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    Ok(result)
}

fn docs_output_inventory(root: &Path) -> Result<Vec<OutputFile>, WakeError> {
    collect_output_tree_files(root, "documentation", false)?
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
                OutputFileKind::Manifest
            } else {
                match extension {
                    "html" => OutputFileKind::Html,
                    "js" | "mjs" | "cjs" => OutputFileKind::Chunk,
                    "map" => OutputFileKind::SourceMap,
                    _ => OutputFileKind::Asset,
                }
            };
            Ok(OutputFile {
                path: relative.to_string_lossy().replace('\\', "/"),
                kind,
                bytes,
            })
        })
        .collect()
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

#[cfg(test)]
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
    commit_staged_output_with(
        staging,
        target,
        owned_roots,
        product,
        backup_prefix,
        || Ok(()),
        None,
    )
}

fn commit_staged_output_with(
    staging: &Path,
    target: &Path,
    owned_roots: Option<&[&str]>,
    product: &str,
    backup_prefix: &str,
    revalidate: impl FnOnce() -> Result<(), WakeError>,
    fail_after_installs: Option<usize>,
) -> Result<(), WakeError> {
    let target_identity = resolve_physical_output_path(target)?;
    let output_scopes = resolve_directory_output_scopes(&target_identity, owned_roots)?;
    let commit_lock = acquire_output_commit_lock(product)?;
    let locked_identity = resolve_physical_output_path(target)?;
    if locked_identity != target_identity {
        return Err(WakeError::new(
            "WAKE_OUTPUT_COLLISION",
            "output target identity changed while waiting for its publication lock",
        )
        .at(&locked_identity));
    }
    let locked_scopes = resolve_directory_output_scopes(&locked_identity, owned_roots)?;
    if locked_scopes != output_scopes {
        return Err(WakeError::new(
            "WAKE_OUTPUT_COLLISION",
            "output mutation scope identity changed while waiting for its publication lock",
        )
        .at(&locked_identity));
    }
    validate_directory_output_commit_scope(&locked_scopes, commit_lock.lock_paths(), product)?;
    revalidate()?;
    commit_staged_output_locked(
        staging,
        &locked_identity,
        owned_roots,
        product,
        backup_prefix,
        fail_after_installs,
    )
}

fn resolve_directory_output_scopes(
    target: &Path,
    owned_roots: Option<&[&str]>,
) -> Result<Vec<PathBuf>, WakeError> {
    let mut scopes = owned_roots
        .map(|roots| {
            roots
                .iter()
                .map(|root| resolve_physical_output_path(&target.join(root)))
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_else(|| Ok(vec![resolve_physical_output_path(target)?]))?;
    if scopes.is_empty() {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            "directory output commit requires at least one mutation scope",
        ));
    }
    scopes.sort();
    scopes.dedup();
    Ok(scopes)
}

fn validate_directory_output_commit_scope(
    scopes: &[PathBuf],
    lock_paths: &[PathBuf],
    product: &str,
) -> Result<(), WakeError> {
    for lock_path in lock_paths {
        let lock_path = resolve_physical_output_path(lock_path)?;
        for scope in scopes {
            if lock_path.starts_with(scope) || scope.starts_with(&lock_path) {
                return Err(WakeError::new(
                    "WAKE_OUTPUT_COLLISION",
                    format!(
                        "refusing to publish {product} output over a live Wake output-commit lock"
                    ),
                )
                .at(scope));
            }
        }
    }
    Ok(())
}

fn commit_staged_output_locked(
    staging: &Path,
    target: &Path,
    owned_roots: Option<&[&str]>,
    product: &str,
    backup_prefix: &str,
    fail_after_installs: Option<usize>,
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
    let staged_files = collect_scoped_output_files(staging, owned_roots, product, true)?;
    let existing_files = collect_scoped_output_files(target, owned_roots, product, false)?;
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
    for (installs, (relative, bytes)) in replacements.into_iter().enumerate() {
        let destination = target.join(&relative);
        if fail_after_installs == Some(installs) {
            return Err(rollback_output_tree(
                target,
                backup.path(),
                &touched,
                WakeError::new("WAKE_IO", "injected directory output install failure")
                    .at(&destination),
            ));
        }
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
    reject_commit_locks: bool,
) -> Result<Vec<PathBuf>, WakeError> {
    let Some(owned_roots) = owned_roots else {
        return collect_output_tree_files(base, product, reject_commit_locks);
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
            collect_output_tree_files(&directory, product, reject_commit_locks)?
                .into_iter()
                .map(|relative| PathBuf::from(owned).join(relative)),
        );
    }
    files.sort();
    Ok(files)
}

fn collect_output_tree_files(
    base: &Path,
    product: &str,
    reject_commit_locks: bool,
) -> Result<Vec<PathBuf>, WakeError> {
    fn visit(
        base: &Path,
        directory: &Path,
        files: &mut Vec<PathBuf>,
        product: &str,
        reject_commit_locks: bool,
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
            if is_output_commit_lock_path(&path) {
                if reject_commit_locks {
                    return Err(WakeError::new(
                        "WAKE_OUTPUT_COLLISION",
                        "staged directory output uses Wake's reserved output-commit lock name",
                    )
                    .at(&path));
                }
                if metadata_is_link_or_reparse_point(&metadata) || !metadata.is_file() {
                    return Err(WakeError::new(
                        "WAKE_CONFIG",
                        "invalid Wake output-commit lock metadata",
                    )
                    .at(&path));
                }
                continue;
            }
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
                visit(base, &path, files, product, reject_commit_locks)?;
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
    visit(base, base, &mut files, product, reject_commit_locks)?;
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
    let dev_options = options.clone();
    let docs_options = DocsBuildOptions {
        project: options.project.clone(),
        outdir: None,
        base_path: None,
        presentation: None,
    };
    let (prepared, docs, _routes, _demos, warnings, _changed_files) =
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
    let forwarded_tx = event_tx.clone();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        forward_dev_server_event(&forwarded_tx, event);
    });
    let docs_base_path = docs.base_path.clone();
    let watch_interests = docs_watch_interests(&prepared, &docs);
    let refresh = docs_refresh(
        docs_options,
        dev_options,
        &prepared,
        &docs,
        docs_mode,
        true,
        None,
        event_tx.clone(),
        None,
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
        file_system: Some(prepared.generation.file_system()),
        watch_interests,
        refresh: Some(refresh),
        quiet: true,
        event_handler: Some(event_handler),
        mounts: Vec::new(),
        deferred_mounts: Vec::new(),
        federation: wake_dev_server::FederationBuildOptions::default(),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
        federation_type_monitor: None,
    })
}

fn start_aggregated_docs_dev_server(
    options: DevServerOptions,
    docs_options: DocsBuildOptions,
    workspaces: Vec<ResolvedDocsWorkspace>,
) -> Result<DevServer, WakeError> {
    let dev_options = options.clone();
    let (prepared, site_docs, _routes, _demos, warnings, _changed_files) = prepare_docs(
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
    let forwarded_tx = event_tx.clone();
    let event_handler: wake_dev_server::EventHandler = Arc::new(move |event| {
        forward_dev_server_event(&forwarded_tx, event);
    });

    let topology = docs_workspace_topology(&workspaces);
    let site_refresh = docs_refresh(
        docs_options.clone(),
        dev_options.clone(),
        &prepared,
        &site_docs,
        DocsMode::Site,
        true,
        Some(topology),
        event_tx.clone(),
        None,
    );

    let mut mounts = Vec::new();
    let mut deferred_mounts = Vec::new();
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
        if workspace.dev_loading == wake_config::DocsWorkspaceDevLoading::Lazy {
            let probe = probe_docs_candidate(&workspace_options, DocsMode::Components)
                .map_err(|error| scope_workspace_error(error, &workspace.name, &workspace.root))?;
            let watch_interests = docs_probe_watch_interests(&probe);
            let refresh = deferred_docs_refresh(
                workspace_options,
                dev_options.clone(),
                probe.clone(),
                DocsMode::Components,
                event_tx.clone(),
                Some(workspace.name.clone()),
            );
            deferred_mounts.push(wake_dev_server::DeferredMountedServeOptions {
                name: workspace.name,
                root: probe.prepared.root,
                base_path: workspace.base_path,
                watch_interests,
                refresh,
                federation: wake_dev_server::FederationBuildOptions::default(),
            });
            continue;
        }
        let (workspace_prepared, workspace_docs, _routes, _demos, warnings, _changed_files) =
            prepare_docs(
                &workspace_options,
                wake_docs::BuildMode::Development,
                DocsMode::Components,
            )
            .map_err(|error| scope_workspace_error(error, &workspace.name, &workspace.root))?;
        send_docs_warnings(&event_tx, warnings, Some(&workspace.name));
        let workspace_config = workspace_prepared.config.clone();
        let watch_interests = docs_watch_interests(&workspace_prepared, &workspace_docs);
        let refresh = docs_refresh(
            workspace_options,
            dev_options.clone(),
            &workspace_prepared,
            &workspace_docs,
            DocsMode::Components,
            false,
            None,
            event_tx.clone(),
            Some(workspace.name.clone()),
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
            loading: wake_dev_server::DevLoading::Eager,
            entry: workspace_prepared.entry,
            resolve_options,
            define: build_defines(&workspace_config, true),
            target_env,
            jsx_import_source: workspace_config.react.jsx_import_source.clone(),
            file_system: Some(workspace_prepared.generation.file_system()),
            watch_interests,
            refresh: Some(refresh),
            federation: wake_dev_server::FederationBuildOptions::default(),
        });
    }

    let site_watch_interests = docs_watch_interests(&prepared, &site_docs);
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
        file_system: Some(prepared.generation.file_system()),
        watch_interests: site_watch_interests,
        refresh: Some(site_refresh),
        quiet: true,
        event_handler: Some(event_handler),
        mounts,
        deferred_mounts,
        federation: wake_dev_server::FederationBuildOptions::default(),
    };
    let handle = wake_dev_server::start(&prepared.root, port, serve_options)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    Ok(DevServer {
        handle,
        events: Arc::new(Mutex::new(event_rx)),
        federation_type_monitor: None,
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocsDevTopology {
    config_dir: PathBuf,
    root: PathBuf,
    entry: PathBuf,
    source_dir: PathBuf,
    preview: Option<PathBuf>,
    theme_css: Option<PathBuf>,
    base_path: String,
    presentation: DocsPresentation,
    component_scan: Vec<ComponentScanTopology>,
    federation: FederationOptions,
    server: Option<DocsServerTopology>,
}

fn docs_dev_topology(
    prepared: &PreparedBuild,
    docs: &wake_docs::DocsOptions,
    dev_options: &DevServerOptions,
    owns_server: bool,
) -> DocsDevTopology {
    docs_dev_topology_from(
        &prepared.config_dir,
        &prepared.root,
        &prepared.logical_entry,
        &prepared.config,
        docs,
        dev_options,
        owns_server,
    )
}

fn docs_probe_topology(
    prepared: &PreparedDocsProbe,
    dev_options: &DevServerOptions,
    owns_server: bool,
) -> DocsDevTopology {
    docs_dev_topology_from(
        &prepared.prepared.config_dir,
        &prepared.prepared.root,
        &prepared.prepared.logical_entry,
        &prepared.prepared.config,
        &prepared.docs,
        dev_options,
        owns_server,
    )
}

fn docs_dev_topology_from(
    config_dir: &Path,
    root: &Path,
    logical_entry: &Path,
    config: &wake_config::Config,
    docs: &wake_docs::DocsOptions,
    dev_options: &DevServerOptions,
    owns_server: bool,
) -> DocsDevTopology {
    let server = &config.dev_server;
    DocsDevTopology {
        config_dir: config_dir.to_path_buf(),
        root: root.to_path_buf(),
        entry: logical_entry.to_path_buf(),
        source_dir: docs.source_dir.clone(),
        preview: docs.preview.clone(),
        theme_css: docs.theme_css.clone(),
        base_path: docs.base_path.clone(),
        presentation: docs.presentation,
        component_scan: config
            .component_scan
            .iter()
            .map(|rule| {
                (
                    rule.namespace.clone(),
                    rule.cwd.clone(),
                    rule.generate_source,
                    rule.include.clone(),
                    rule.exclude.clone(),
                )
            })
            .collect(),
        federation: config.federation.clone(),
        server: owns_server.then(|| {
            (
                server.server.clone(),
                dev_options.port.or(server.port).unwrap_or(5173),
                dev_options
                    .host
                    .clone()
                    .or_else(|| server.host.clone())
                    .unwrap_or_else(|| "127.0.0.1".to_owned()),
                dev_options.open.unwrap_or(server.open),
                server
                    .proxy
                    .iter()
                    .map(|proxy| {
                        (
                            proxy.context.clone(),
                            proxy.target.clone(),
                            proxy.ws,
                            proxy.change_origin,
                            proxy
                                .path_rewrite
                                .iter()
                                .map(|(pattern, replacement)| {
                                    (pattern.clone(), replacement.clone())
                                })
                                .collect(),
                        )
                    })
                    .collect(),
            )
        }),
    }
}

fn docs_topology_change(current: &DocsDevTopology, candidate: &DocsDevTopology) -> Option<String> {
    if current.config_dir != candidate.config_dir {
        return Some("Docs configuration source changed".to_owned());
    }
    if current.root != candidate.root {
        return Some("Docs project root changed".to_owned());
    }
    if current.entry != candidate.entry {
        return Some("Docs generated entry topology changed".to_owned());
    }
    if current.source_dir != candidate.source_dir
        || current.preview != candidate.preview
        || current.theme_css != candidate.theme_css
    {
        return Some("Docs source, Preview, or theme topology changed".to_owned());
    }
    if current.base_path != candidate.base_path || current.presentation != candidate.presentation {
        return Some("Docs mount URL or presentation changed".to_owned());
    }
    if current.component_scan != candidate.component_scan {
        return Some("Docs component scan topology changed".to_owned());
    }
    if current.federation != candidate.federation {
        return Some("Federation topology changed".to_owned());
    }
    if current.server != candidate.server {
        return Some("Docs server or proxy topology changed".to_owned());
    }
    None
}

#[allow(clippy::result_large_err)]
fn make_docs_refresh_candidate(
    state: Arc<Mutex<RefreshState<Option<PreparedDocsRefresh>, PreparedDocsProbe>>>,
    id: u64,
    draft: PreparedDocsProbe,
    docs_mode: DocsMode,
    event_tx: mpsc::Sender<DevServerEvent>,
    workspace: Option<String>,
) -> wake_dev_server::DevMountCandidate {
    let preliminary_interests = docs_probe_watch_interests(&draft);
    let accepted_slot = Arc::new(Mutex::new(None::<PreparedDocsRefresh>));
    let materialize_slot = Arc::clone(&accepted_slot);
    let materialize_state = Arc::clone(&state);
    let materialize_draft = draft.clone();
    wake_dev_server::DevMountCandidate::new(
        preliminary_interests,
        move || {
            let result = materialize_docs_probe(
                materialize_draft,
                wake_docs::BuildMode::Development,
                docs_mode,
            )
            .and_then(
                |(prepared, docs, _routes, _demos, warnings, _changed_files)| {
                    let changed_files = {
                        let state = materialize_state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        state.accepted.as_ref().map_or_else(
                            || prepared.generation.logical_inventory(),
                            |accepted| {
                                generation_changed_paths(
                                    &accepted.prepared.generation,
                                    &prepared.generation,
                                )
                            },
                        )
                    };
                    let plan =
                        app_dev_plan(&prepared, prepared.entry.clone(), prepared.aliases.clone())?;
                    let watch_interests = docs_watch_interests(&prepared, &docs);
                    Ok((
                        PreparedDocsRefresh {
                            prepared,
                            docs,
                            warnings,
                        },
                        plan,
                        watch_interests,
                        changed_files,
                    ))
                },
            );
            match result {
                Ok((prepared, plan, watch_interests, generated_paths)) => {
                    *materialize_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(prepared);
                    Ok(wake_dev_server::DevMountMaterialization {
                        plan,
                        watch_interests,
                        generated_paths,
                    })
                }
                Err(error) => {
                    let diagnostic = wake_error_diagnostic(error);
                    let mut state = materialize_state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if let CandidateState::Pending {
                        id: current_id,
                        last_error,
                        ..
                    } = &mut state.candidate
                        && *current_id == id
                    {
                        *last_error = Some(diagnostic.clone());
                    }
                    Err(diagnostic)
                }
            }
        },
        move |outcome| {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let current = matches!(
                &state.candidate,
                CandidateState::Pending { id: current_id, .. } if *current_id == id
            );
            if !current {
                return;
            }
            match outcome {
                wake_dev_server::RefreshOutcome::Committed => {
                    if let Some(accepted) = accepted_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .take()
                    {
                        send_docs_warnings(
                            &event_tx,
                            accepted.warnings.clone(),
                            workspace.as_deref(),
                        );
                        state.accepted = Some(accepted);
                        state.candidate = CandidateState::Stable;
                    }
                }
                wake_dev_server::RefreshOutcome::Superseded => {
                    state.candidate = CandidateState::Stable;
                }
                wake_dev_server::RefreshOutcome::RetryableFailure
                | wake_dev_server::RefreshOutcome::Aborted => {}
            }
        },
    )
}

#[allow(clippy::result_large_err)]
fn docs_refresh(
    options: DocsBuildOptions,
    dev_options: DevServerOptions,
    prepared: &PreparedBuild,
    docs: &wake_docs::DocsOptions,
    docs_mode: DocsMode,
    owns_server: bool,
    expected_workspaces: Option<DocsWorkspaceTopology>,
    event_tx: mpsc::Sender<DevServerEvent>,
    workspace: Option<String>,
) -> wake_dev_server::RefreshMount {
    let initial_topology = docs_dev_topology(prepared, docs, &dev_options, owns_server);
    let config_interest =
        WatchInterest::exact_file(prepared.config_dir.join(wake_config::CONFIG_FILE))
            .resolve_against(&prepared.root);
    let federation_lock_interest =
        WatchInterest::exact_file(prepared.root.join("wake-federation.lock"))
            .resolve_against(&prepared.root);
    let initial_federation_lock_fingerprint =
        prepared_control_fingerprint(prepared, &prepared.root.join("wake-federation.lock"));
    let control_interests = project_control_interests(prepared);
    let state: Arc<Mutex<RefreshState<Option<PreparedDocsRefresh>, PreparedDocsProbe>>> =
        Arc::new(Mutex::new(RefreshState {
            accepted: Some(PreparedDocsRefresh {
                prepared: prepared.clone(),
                docs: docs.clone(),
                warnings: Vec::new(),
            }),
            next_id: 1,
            candidate: CandidateState::Stable,
        }));
    Arc::new(move |_current, invalidation| {
        let changed = invalidation.paths();
        let rescan = invalidation.is_rescan();
        let accepted_root = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .accepted
            .as_ref()
            .expect("ready Docs refresh owns an accepted generation")
            .prepared
            .root
            .clone();
        let current_controls = control_interests
            .iter()
            .map(|interest| interest.resolve_against(&accepted_root))
            .collect::<Vec<_>>();
        let current_config_interest = config_interest.resolve_against(&accepted_root);
        let current_federation_lock = federation_lock_interest.resolve_against(&accepted_root);
        let federation_lock_changed = federation_lock_changed(
            changed,
            rescan,
            &current_federation_lock,
            &accepted_root.join("wake-federation.lock"),
            &initial_federation_lock_fingerprint,
        );
        if federation_lock_changed {
            let diagnostic =
                wake_error_diagnostic(restart_required_error("Federation lock changed"));
            let mut refresh_state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            refresh_state.candidate = CandidateState::Blocked { diagnostic };
            return Ok(wake_dev_server::DevMountRefresh::RestartRequired {
                reason: "Federation lock changed".to_owned(),
            });
        }
        let control_changed = rescan || changed_matches(changed, &current_controls);
        if !control_changed {
            let state_guard = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match &state_guard.candidate {
                CandidateState::Blocked { diagnostic } => {
                    let accepted = state_guard
                        .accepted
                        .as_ref()
                        .expect("ready Docs refresh owns an accepted generation");
                    return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                        watch_interests: docs_watch_interests(&accepted.prepared, &accepted.docs),
                        diagnostic: diagnostic.clone(),
                    });
                }
                CandidateState::Pending { id, draft, .. } => {
                    let id = *id;
                    let draft = draft.clone();
                    drop(state_guard);
                    return Ok(wake_dev_server::DevMountRefresh::Candidate(
                        make_docs_refresh_candidate(
                            Arc::clone(&state),
                            id,
                            draft,
                            docs_mode,
                            event_tx.clone(),
                            workspace.clone(),
                        ),
                    ));
                }
                CandidateState::Stable => {}
            }
            let draft = docs_probe_from_refresh(
                state_guard
                    .accepted
                    .as_ref()
                    .expect("ready Docs refresh owns an accepted generation"),
            );
            drop(state_guard);
            let id = {
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let id = state.next_id;
                state.next_id = state.next_id.wrapping_add(1).max(1);
                state.candidate = CandidateState::Pending {
                    id,
                    draft: draft.clone(),
                    last_error: None,
                };
                id
            };
            return Ok(wake_dev_server::DevMountRefresh::Candidate(
                make_docs_refresh_candidate(
                    Arc::clone(&state),
                    id,
                    draft,
                    docs_mode,
                    event_tx.clone(),
                    workspace.clone(),
                ),
            ));
        }
        if (rescan || changed_matches(changed, std::slice::from_ref(&current_config_interest)))
            && let Some(expected) = &expected_workspaces
        {
            let discovered = match discover_docs_workspaces(&options) {
                Ok(discovered) => discovered,
                Err(error) => {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let accepted = state
                        .accepted
                        .as_ref()
                        .expect("ready Docs refresh owns an accepted generation");
                    let watch_interests = recovery_watch_interests(
                        docs_watch_interests(&accepted.prepared, &accepted.docs),
                        &accepted.prepared.root,
                        &error,
                    );
                    let diagnostic = wake_error_diagnostic(error);
                    state.candidate = CandidateState::Blocked {
                        diagnostic: diagnostic.clone(),
                    };
                    return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                        watch_interests,
                        diagnostic,
                    });
                }
            };
            if docs_workspace_topology(&discovered) != *expected {
                let diagnostic = wake_error_diagnostic(restart_required_error(
                    "Docs workspace topology changed",
                ));
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let accepted = state
                    .accepted
                    .as_ref()
                    .expect("ready Docs refresh owns an accepted generation");
                let watch_interests = docs_watch_interests(&accepted.prepared, &accepted.docs);
                state.candidate = CandidateState::Blocked {
                    diagnostic: diagnostic.clone(),
                };
                return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                });
            }
        }
        let refreshed = match probe_docs_candidate(&options, docs_mode) {
            Ok(prepared) => prepared,
            Err(error) => {
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let accepted = state
                    .accepted
                    .as_ref()
                    .expect("ready Docs refresh owns an accepted generation");
                let watch_interests = recovery_watch_interests(
                    docs_watch_interests(&accepted.prepared, &accepted.docs),
                    &accepted.prepared.root,
                    &error,
                );
                let diagnostic = wake_error_diagnostic(error);
                state.candidate = CandidateState::Blocked {
                    diagnostic: diagnostic.clone(),
                };
                return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                });
            }
        };
        let topology = docs_probe_topology(&refreshed, &dev_options, owns_server);
        if let Some(reason) = docs_topology_change(&initial_topology, &topology) {
            let diagnostic = wake_error_diagnostic(restart_required_error(reason));
            let watch_interests = docs_probe_watch_interests(&refreshed);
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .candidate = CandidateState::Blocked {
                diagnostic: diagnostic.clone(),
            };
            return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                watch_interests,
                diagnostic,
            });
        }
        let id = {
            let mut refresh_state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = refresh_state.next_id;
            refresh_state.next_id = refresh_state.next_id.wrapping_add(1).max(1);
            refresh_state.candidate = CandidateState::Pending {
                id,
                draft: refreshed.clone(),
                last_error: None,
            };
            id
        };
        Ok(wake_dev_server::DevMountRefresh::Candidate(
            make_docs_refresh_candidate(
                Arc::clone(&state),
                id,
                refreshed,
                docs_mode,
                event_tx.clone(),
                workspace.clone(),
            ),
        ))
    })
}

#[allow(clippy::result_large_err)]
fn deferred_docs_refresh(
    options: DocsBuildOptions,
    dev_options: DevServerOptions,
    initial_probe: PreparedDocsProbe,
    docs_mode: DocsMode,
    event_tx: mpsc::Sender<DevServerEvent>,
    workspace: Option<String>,
) -> wake_dev_server::DeferredRefreshMount {
    let root = initial_probe.prepared.root.clone();
    let initial_topology = docs_probe_topology(&initial_probe, &dev_options, false);
    let federation_lock_path = root.join("wake-federation.lock");
    let federation_lock_interest =
        WatchInterest::exact_file(&federation_lock_path).resolve_against(&root);
    let initial_federation_lock_fingerprint = captured_control_fingerprint(
        &initial_probe.prepared.control_fingerprints,
        &federation_lock_path,
    );
    let state: Arc<Mutex<RefreshState<Option<PreparedDocsRefresh>, PreparedDocsProbe>>> =
        Arc::new(Mutex::new(RefreshState {
            accepted: None,
            next_id: 1,
            candidate: CandidateState::Stable,
        }));
    Arc::new(move |invalidation| {
        if federation_lock_changed(
            invalidation.paths(),
            invalidation.is_rescan(),
            &federation_lock_interest,
            &federation_lock_path,
            &initial_federation_lock_fingerprint,
        ) {
            let diagnostic =
                wake_error_diagnostic(restart_required_error("Federation lock changed"));
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .candidate = CandidateState::Blocked { diagnostic };
            return Ok(wake_dev_server::DevMountRefresh::RestartRequired {
                reason: "Federation lock changed".to_owned(),
            });
        }

        let refreshed = match probe_docs_candidate(&options, docs_mode) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                let mut state = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let base = state.accepted.as_ref().map_or_else(
                    || docs_probe_watch_interests(&initial_probe),
                    |accepted| docs_watch_interests(&accepted.prepared, &accepted.docs),
                );
                let watch_interests = recovery_watch_interests(base, &root, &error);
                let diagnostic = wake_error_diagnostic(error);
                state.candidate = CandidateState::Blocked {
                    diagnostic: diagnostic.clone(),
                };
                return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                });
            }
        };
        if let Some(reason) = docs_topology_change(
            &initial_topology,
            &docs_probe_topology(&refreshed, &dev_options, false),
        ) {
            let diagnostic = wake_error_diagnostic(restart_required_error(reason));
            let watch_interests = docs_probe_watch_interests(&refreshed);
            state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .candidate = CandidateState::Blocked {
                diagnostic: diagnostic.clone(),
            };
            return Ok(wake_dev_server::DevMountRefresh::RejectedCandidate {
                watch_interests,
                diagnostic,
            });
        }
        let id = {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = state.next_id;
            state.next_id = state.next_id.wrapping_add(1).max(1);
            state.candidate = CandidateState::Pending {
                id,
                draft: refreshed.clone(),
                last_error: None,
            };
            id
        };
        Ok(wake_dev_server::DevMountRefresh::Candidate(
            make_docs_refresh_candidate(
                Arc::clone(&state),
                id,
                refreshed,
                docs_mode,
                event_tx.clone(),
                workspace.clone(),
            ),
        ))
    })
}

fn docs_watch_interests(
    prepared: &PreparedBuild,
    docs: &wake_docs::DocsOptions,
) -> Vec<WatchInterest> {
    docs_watch_interests_from(project_watch_interests(prepared), &prepared.root, docs)
}

fn docs_probe_watch_interests(prepared: &PreparedDocsProbe) -> Vec<WatchInterest> {
    docs_watch_interests_from(
        probe_watch_interests(&prepared.prepared),
        &prepared.prepared.root,
        &prepared.docs,
    )
}

fn docs_watch_interests_from(
    mut interests: Vec<WatchInterest>,
    root: &Path,
    docs: &wake_docs::DocsOptions,
) -> Vec<WatchInterest> {
    interests.extend([
        WatchInterest::tree(root.join(&docs.source_dir)),
        WatchInterest::exact_file(root.join(&docs.source_dir).join("navigation.toml")),
    ]);
    if let Some(preview) = &docs.preview {
        interests.push(WatchInterest::tree(root.join(preview)));
    }
    if let Some(theme_css) = &docs.theme_css {
        interests.push(WatchInterest::tree(root.join(theme_css)));
    }
    interests = interests
        .into_iter()
        .map(|interest| interest.resolve_against(root))
        .collect();
    interests.sort();
    interests.dedup();
    interests
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

fn docs_workspace_topology(workspaces: &[ResolvedDocsWorkspace]) -> DocsWorkspaceTopology {
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
    materialize_docs_probe(probe_docs_candidate(options, docs_mode)?, mode, docs_mode)
}

fn probe_docs_candidate(
    options: &DocsBuildOptions,
    docs_mode: DocsMode,
) -> Result<PreparedDocsProbe, WakeError> {
    let mut last_snapshot_error = None;
    for _ in 0..3 {
        match probe_docs_candidate_once(options, docs_mode) {
            Err(error) if error.code == "WAKE_WATCH_SNAPSHOT_CHANGED" => {
                last_snapshot_error = Some(error);
            }
            result => return result,
        }
    }
    Err(last_snapshot_error.unwrap_or_else(|| {
        WakeError::new(
            "WAKE_WATCH_SNAPSHOT_CHANGED",
            "project control files changed while preparing the Docs build",
        )
    }))
}

fn probe_docs_candidate_once(
    options: &DocsBuildOptions,
    docs_mode: DocsMode,
) -> Result<PreparedDocsProbe, WakeError> {
    let cwd = options
        .project
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let config_dir = resolve_config_dir(&cwd, options.project.config_path.as_deref())?;
    let config_before = control_file_fingerprint(&config_dir.join(wake_config::CONFIG_FILE));
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
    let docs = docs_options(
        &config,
        options.base_path.as_deref(),
        options.presentation.unwrap_or_default(),
    );
    validate_reserved_docs_inputs(&config, &root, &docs)?;
    let entry_relative = match docs_mode {
        DocsMode::Site => "runtime/site-entry.tsx",
        DocsMode::Components => "runtime/components-entry.tsx",
    };
    let logical_entry = root.join(".wake/docs/generated").join(entry_relative);
    let control_fingerprints =
        stable_project_control_snapshot(&config_before, &config_dir, &root, None)?;
    Ok(PreparedDocsProbe {
        prepared: PreparedBuildProbe {
            config_dir,
            root: root.clone(),
            logical_entry,
            explicit_entry: None,
            outdir: root.join("docs-dist"),
            config,
            control_fingerprints,
        },
        docs,
    })
}

fn materialize_docs_probe(
    probe: PreparedDocsProbe,
    mode: wake_docs::BuildMode,
    docs_mode: DocsMode,
) -> Result<PreparedDocs, WakeError> {
    let PreparedDocsProbe {
        prepared:
            PreparedBuildProbe {
                config_dir,
                root,
                logical_entry: _,
                explicit_entry: _,
                outdir,
                config,
                control_fingerprints,
            },
        docs,
    } = probe;
    let mut generation = GenerationDraft::new(&root);
    let mut aliases = prepare_generation_aliases(&config, &root, &mut generation)?;
    let rendered = wake_docs::render_with_mode(&root, &docs, mode, docs_mode)
        .map_err(|error| WakeError::new("WAKE_BUILD", error.to_string()))?;
    let docs_root = root.join(".wake/docs/generated");
    let changed_files = rendered
        .files
        .inventory()
        .map(|path| docs_root.join(path.as_path()))
        .collect::<Vec<_>>();
    generation.insert_tree(Path::new("docs/generated"), &rendered.files)?;
    aliases.retain(|(name, _)| name != "@@wake/docs" && name != "@@wake/docs-project");
    aliases.extend([
        ("@@wake/docs".to_string(), docs_root.clone()),
        ("@@wake/docs-project".to_string(), root.clone()),
    ]);
    let entry = docs_root.join(rendered.entry_relative.as_path());
    let routes = rendered.routes;
    let demos = rendered.demos;
    let warnings = rendered.warnings;
    let generation = generation.seal()?;
    Ok((
        PreparedBuild {
            config_dir,
            root: root.clone(),
            entry: entry.clone(),
            logical_entry: entry,
            explicit_entry: None,
            outdir,
            config,
            control_fingerprints,
            aliases,
            core_generation: generation.clone(),
            generation,
        },
        docs,
        routes,
        demos,
        warnings,
        changed_files,
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

fn validate_reserved_docs_inputs(
    config: &wake_config::Config,
    root: &Path,
    docs: &wake_docs::DocsOptions,
) -> Result<(), WakeError> {
    for (kind, path) in config
        .alias
        .values()
        .map(|path| ("resolver alias", absolute_from(root, Path::new(path))))
        .chain(config.component_scan.iter().map(|rule| {
            (
                "component scan root",
                absolute_from(root, Path::new(&rule.cwd)),
            )
        }))
        .chain(config.federation.exposes.values().map(|expose| {
            (
                "Federation expose",
                absolute_from(root, Path::new(&expose.entry)),
            )
        }))
    {
        validate_not_reserved(root, kind, &path)?;
    }
    validate_not_reserved(root, "Docs source", &absolute_from(root, &docs.source_dir))?;
    if let Some(preview) = &docs.preview {
        validate_not_reserved(root, "Docs Preview", &absolute_from(root, preview))?;
    }
    if let Some(theme) = &docs.theme_css {
        validate_not_reserved(root, "Docs theme", &absolute_from(root, theme))?;
    }
    Ok(())
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

    #[test]
    fn persistent_cache_path_does_not_create_optional_state() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();

        assert_eq!(persistent_cache_path(root, false, "cache.bin"), None);
        assert_eq!(
            persistent_cache_path(root, true, "cache.bin"),
            Some(root.join(".wake").join("cache.bin"))
        );
        assert!(!root.join(".wake").exists());
    }

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

    fn http_get(port: u16, path: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
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
                kind: wake_test_contract::TestEnvironmentKind::Dom,
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
            &OsFileSystem,
        );
        assert!(missing[0].location.is_none());
    }

    #[test]
    fn diagnostic_locations_read_the_owned_generation_instead_of_shadowed_host_files() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let logical_root = root.join(".wake");
        std::fs::create_dir_all(&logical_root).unwrap();
        std::fs::write(logical_root.join("generated.ts"), "rogue host bytes\n").unwrap();

        let overlay_text = "first line\nconst generated = ;\n";
        let mut files = OwnedFileTreeBuilder::new();
        files
            .insert(
                ProjectedRelativePath::new("generated.ts").unwrap(),
                Arc::<[u8]>::from(overlay_text.as_bytes()),
            )
            .unwrap();
        let file_system =
            OwnedOverlayFileSystem::try_new(Arc::new(OsFileSystem), &logical_root, files.seal())
                .unwrap();
        let start = overlay_text.find("generated").unwrap() as u32;
        let end = start + "generated".len() as u32;
        let infos = diagnostic_infos(
            &[Diagnostic::error("generated source error")
                .with_path(".wake/generated.ts")
                .with_primary(wake_common::Span::new(start, end), "generated binding")],
            root,
            &file_system,
        );

        let location = infos[0].location.as_ref().expect("overlay source location");
        assert_eq!(location.line, 2);
        assert_eq!(location.line_text, "const generated = ;");
        assert_eq!(location.label.as_deref(), Some("generated binding"));
    }

    #[test]
    fn build_defines_keep_the_unsupported_hot_module_api_false() {
        let mut config = wake_config::Config::default();
        config
            .define
            .insert("import.meta.hot".to_owned(), "true".to_owned());
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
        let mut session = BuildSession::new(
            Arc::new(fs),
            BundlerBuildOptions {
                extract_css: true,
                code_splitting: true,
                ..BundlerBuildOptions::default()
            },
        );
        let output = session.build(BuildRequest::new("src/index.js"));
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
    fn absolute_project_outputs_rebase_only_the_declared_root_alias() {
        #[cfg(unix)]
        let (configured, physical, outside) = (
            Path::new("/var/folders/project"),
            Path::new("/private/var/folders/project"),
            Path::new("/opt/wake-output"),
        );
        #[cfg(windows)]
        let (configured, physical, outside) = (
            Path::new(r"C:\Users\RUNNER~1\project"),
            Path::new(r"C:\Users\runneradmin\project"),
            Path::new(r"D:\wake-output"),
        );

        assert_eq!(
            absolute_from_project_root(configured, physical, &configured.join("dist")),
            physical.join("dist")
        );
        assert_eq!(
            absolute_from_project_root(configured, physical, &configured.join("linked/dist")),
            physical.join("linked/dist")
        );
        assert_eq!(
            absolute_from_project_root(configured, physical, outside),
            outside
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_safety_accepts_only_symlink_ancestors_above_the_physical_project() {
        use std::os::unix::fs::symlink;

        let outer = tempfile::tempdir().unwrap();
        let real_parent = outer.path().join("real");
        let project = real_parent.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let alias = outer.path().join("alias");
        symlink(&real_parent, &alias).unwrap();
        let physical_project = canonical_project_root(&project).unwrap();

        assert_eq!(
            resolve_physical_output_path_with_project_root(
                &alias.join("project/dist"),
                Some(&physical_project)
            )
            .unwrap(),
            physical_project.join("dist")
        );

        let internal_alias = project.join("internal-alias");
        symlink(&real_parent, &internal_alias).unwrap();
        let error = resolve_physical_output_path_with_project_root(
            &internal_alias.join("escape"),
            Some(&physical_project),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(error.path.as_deref(), internal_alias.to_str());
    }

    fn generation_seal_count(root: &Path) -> usize {
        let root = canonical_project_root(root).unwrap();
        GENERATION_SEALS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&root)
            .copied()
            .unwrap_or_default()
    }

    fn activate_bootstrap_after_current_coverage(
        bootstrap: &mut BuildWatchBootstrap,
    ) -> BuildContext {
        for _ in 0..4 {
            let revision = bootstrap.watch_plan().revision;
            match bootstrap.activate_at(revision) {
                Ok(context) => return context,
                Err(error) if error.code == "WAKE_WATCH_COVERAGE_PENDING" => {}
                Err(error) => panic!("bootstrap activation failed: {error}"),
            }
        }
        panic!("bootstrap did not converge after current coverage")
    }

    fn bootstrap_activation_error(
        bootstrap: &mut BuildWatchBootstrap,
        revision: WatchPlanRevision,
    ) -> WakeError {
        match bootstrap.activate_at(revision) {
            Err(error) => error,
            Ok(context) => {
                context.close();
                panic!("bootstrap unexpectedly activated")
            }
        }
    }

    #[test]
    fn build_watch_bootstrap_recovers_invalid_toml_without_early_generation() {
        let fixture = Fixture::new("build-watch-bootstrap-invalid-toml");
        fixture.write(wake_config::CONFIG_FILE, "[html\n");
        let options = BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        };
        let mut bootstrap = BuildWatchBootstrap::create(options).unwrap();
        let initial = bootstrap.watch_plan();
        let config = fixture.0.join(wake_config::CONFIG_FILE);
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Waiting { .. }
        ));
        assert!(
            initial
                .interests
                .iter()
                .any(|interest| interest.matches_exact_file(&config))
        );
        assert!(!fixture.0.join(".wake").exists());
        let error = bootstrap_activation_error(&mut bootstrap, initial.revision);
        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(bootstrap.watch_plan().revision, initial.revision);
        assert!(!fixture.0.join(".wake").exists());

        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n",
        );
        assert_eq!(
            bootstrap_activation_error(&mut bootstrap, initial.revision).code,
            "WAKE_WATCH_COVERAGE_PENDING"
        );
        assert!(!fixture.0.join(".wake").exists());
        let context = activate_bootstrap_after_current_coverage(&mut bootstrap);
        let plan = context.watch_plan();
        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    plan.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        context.close();
    }

    #[test]
    fn build_watch_bootstrap_recovers_missing_config_entry_and_root() {
        let fixture = Fixture::new("build-watch-bootstrap-missing-facts");
        let config = fixture.0.join(wake_config::CONFIG_FILE);
        std::fs::remove_file(&config).unwrap();
        let options = BuildOptions {
            project: ProjectOptions {
                cwd: Some(fixture.0.clone()),
                config_path: Some(config.clone()),
            },
            write: false,
            ..BuildOptions::default()
        };
        let mut bootstrap = BuildWatchBootstrap::create(options).unwrap();
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Waiting { .. }
        ));
        assert!(
            bootstrap
                .watch_plan()
                .interests
                .iter()
                .any(|interest| interest.matches_exact_file(&config))
        );
        assert!(!fixture.0.join(".wake").exists());

        fixture.write(
            wake_config::CONFIG_FILE,
            "root_dir = \"project\"\n[html]\nentry = \"src/index.js\"\n",
        );
        let revision = bootstrap.watch_plan().revision;
        let missing_root = fixture.0.join("project");
        let error = bootstrap_activation_error(&mut bootstrap, revision);
        assert_eq!(error.code, "WAKE_CONFIG");
        assert!(
            bootstrap
                .watch_plan()
                .interests
                .iter()
                .any(|interest| interest.matches_event(&missing_root, true))
        );
        assert!(!fixture.0.join(".wake").exists());

        fixture.write("project/src/index.js", "export const recovered = true;\n");
        let context = activate_bootstrap_after_current_coverage(&mut bootstrap);
        context.close();

        let missing_entry = Fixture::new("build-watch-bootstrap-missing-entry");
        std::fs::remove_file(missing_entry.0.join("src/index.js")).unwrap();
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: missing_entry.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let entry = missing_entry.0.join("src/index.js");
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Waiting { .. }
        ));
        assert!(
            bootstrap
                .watch_plan()
                .interests
                .iter()
                .any(|interest| interest.matches_event(&entry, true))
        );
        assert!(!missing_entry.0.join(".wake").exists());
        missing_entry.write("src/index.js", "export const recovered = true;\n");
        activate_bootstrap_after_current_coverage(&mut bootstrap).close();
    }

    #[test]
    fn build_watch_bootstrap_tracks_implicit_discovery_markers() {
        let fixture = Fixture::new("build-watch-bootstrap-discovery");
        let child = fixture.0.join("packages/client");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::remove_file(fixture.0.join(wake_config::CONFIG_FILE)).unwrap();
        fixture.write("package.json", "{}\n");
        let bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: ProjectOptions {
                cwd: Some(child.clone()),
                config_path: None,
            },
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let plan = bootstrap.watch_plan();
        for marker in [
            child.join(wake_config::CONFIG_FILE),
            child.join("package.json"),
            fixture.0.join(wake_config::CONFIG_FILE),
            fixture.0.join("package.json"),
        ] {
            assert!(
                plan.interests
                    .iter()
                    .any(|interest| interest.matches_exact_file(&marker)),
                "missing discovery marker {}",
                marker.display()
            );
        }
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Waiting { .. }
        ));
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn build_context_retains_discovery_floor_after_bootstrap_handoff() {
        let fixture = Fixture::new("build-watch-context-discovery-floor");
        let child = fixture.0.join("packages/client");
        std::fs::create_dir_all(&child).unwrap();
        let closer_config = child.join(wake_config::CONFIG_FILE);
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: ProjectOptions {
                cwd: Some(child.clone()),
                config_path: None,
            },
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Activatable { .. }
        ));
        let context = activate_bootstrap_after_current_coverage(&mut bootstrap);
        let initial = context.watch_plan();
        assert!(
            initial
                .interests
                .iter()
                .any(|interest| interest.matches_exact_file(&closer_config)),
            "the retained context must own missing closer discovery markers"
        );
        context
            .rebuild_watch_at(
                WatchInvalidation::Rescan,
                initial.revision,
                CancellationToken::default(),
            )
            .unwrap();
        assert!(
            context
                .watch_plan()
                .interests
                .iter()
                .any(|interest| interest.matches_exact_file(&closer_config)),
            "a successful build must not shrink away the discovery floor"
        );

        fixture.write(
            "packages/client/wake.config.toml",
            "root_dir = \"../..\"\n[html]\nentry = \"src/index.js\"\n",
        );
        let current = context.watch_plan();
        let error = context
            .rebuild_watch_at(
                WatchInvalidation::Paths(vec![closer_config]),
                current.revision,
                CancellationToken::default(),
            )
            .unwrap_err();
        assert_eq!(error.code, "WAKE_DEV_RESTART_REQUIRED");
        context.close();
    }

    #[test]
    fn build_watch_bootstrap_plan_covers_derived_inputs_and_excludes_output() {
        let fixture = Fixture::new("build-watch-bootstrap-derived-plan");
        std::fs::remove_dir_all(fixture.0.join("src")).unwrap();
        fixture.write("index.js", "export const value = true;\n");
        fixture.write("scan/page.ts", "export const page = true;\n");
        fixture.write(
            "packages/Button.tsx",
            "export default function Button() {}\n",
        );
        fixture.write(
            wake_config::CONFIG_FILE,
            r#"[html]
entry = "index.js"

[alias]
external = "linked-package"

[[component_scan]]
namespace = "pages"
cwd = "scan"
generate_source = true

[federation]
enabled = true
name = "shell"

[federation.exposes."./Button"]
entry = "packages/Button.tsx"
"#,
        );
        let linked = fixture.0.join("linked-package");
        let target = fixture.0.join("actual-package");
        std::fs::create_dir_all(&target).unwrap();
        #[cfg(unix)]
        let linked_created = std::os::unix::fs::symlink(&target, &linked).is_ok();
        #[cfg(windows)]
        let linked_created = std::os::windows::fs::symlink_dir(&target, &linked).is_ok();
        if !linked_created {
            std::fs::create_dir_all(&linked).unwrap();
        }

        let bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist")),
            write: true,
            ..BuildOptions::default()
        })
        .unwrap();
        let plan = bootstrap.watch_plan();
        for input in [
            linked.clone(),
            fixture.0.join("scan"),
            fixture.0.join("packages/Button.tsx"),
        ] {
            assert!(
                plan.interests
                    .iter()
                    .any(|interest| interest.matches_event(&input, true)),
                "missing derived input {}",
                input.display()
            );
        }
        if linked_created {
            assert!(
                plan.interests
                    .iter()
                    .any(|interest| interest.matches_event(&target.join("change.ts"), true)),
                "resolved symlink identity is not covered"
            );
        }
        assert!(
            !plan
                .interests
                .iter()
                .any(|interest| interest.matches_event(&fixture.0.join("dist/chunk.js"), true))
        );
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn build_watch_bootstrap_recovers_a_missing_explicit_entry() {
        let fixture = Fixture::new("build-watch-bootstrap-explicit-entry");
        let entry = fixture.0.join("external/missing.js");
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: fixture.project(),
            entry: Some(entry.clone()),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        assert!(matches!(
            bootstrap.state(),
            BuildWatchBootstrapState::Waiting { .. }
        ));
        assert!(
            bootstrap
                .watch_plan()
                .interests
                .iter()
                .any(|interest| interest.matches_event(&entry, true))
        );
        assert!(!fixture.0.join(".wake").exists());
        fixture.write("external/missing.js", "export const recovered = true;\n");
        activate_bootstrap_after_current_coverage(&mut bootstrap).close();
    }

    #[test]
    fn build_watch_bootstrap_revisions_are_semantic_and_stale_is_side_effect_free() {
        let fixture = Fixture::new("build-watch-bootstrap-revision");
        std::fs::remove_file(fixture.0.join("src/index.js")).unwrap();
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("output")),
            ..BuildOptions::default()
        })
        .unwrap();
        let initial = bootstrap.watch_plan();
        assert_eq!(
            bootstrap_activation_error(&mut bootstrap, WatchPlanRevision(initial.revision.0 + 1),)
                .code,
            "WAKE_WATCH_COVERAGE_PENDING"
        );
        assert_eq!(bootstrap.watch_plan(), initial);
        assert!(!fixture.0.join(".wake").exists());
        assert!(!fixture.0.join("output").exists());

        let error = bootstrap_activation_error(&mut bootstrap, initial.revision);
        assert_eq!(error.code, "WAKE_IO");
        assert_eq!(bootstrap.watch_plan(), initial);
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn build_watch_bootstrap_rejects_only_nonrecoverable_constructor_inputs() {
        let fixture = Fixture::new("build-watch-bootstrap-fatal-inputs");
        assert_eq!(
            BuildWatchBootstrap::create(BuildOptions {
                project: ProjectOptions {
                    cwd: Some(fixture.0.join("does-not-exist")),
                    config_path: None,
                },
                ..BuildOptions::default()
            })
            .err()
            .expect("invalid cwd is fatal")
            .code,
            "WAKE_CONFIG"
        );
        assert_eq!(
            BuildWatchBootstrap::create(BuildOptions {
                project: ProjectOptions {
                    cwd: Some(fixture.0.clone()),
                    config_path: Some(PathBuf::from("other.toml")),
                },
                ..BuildOptions::default()
            })
            .err()
            .expect("bad config basename is fatal")
            .code,
            "WAKE_CONFIG"
        );
        assert_eq!(
            BuildWatchBootstrap::create(BuildOptions {
                project: fixture.project(),
                federation: Some(FederationOptions {
                    enabled: true,
                    ..FederationOptions::default()
                }),
                ..BuildOptions::default()
            })
            .err()
            .expect("invalid programmatic federation is fatal")
            .code,
            "FED_CONFIG_INVALID"
        );
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn build_watch_bootstrap_refined_widening_requires_new_coverage() {
        let fixture = Fixture::new("build-watch-bootstrap-refined-fence");
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let initial = bootstrap.watch_plan();
        let refined = fixture.0.join("resolved-after-materialization");
        bootstrap.refined_interest_for_test = Some(WatchInterest::tree(&refined));

        assert_eq!(
            bootstrap_activation_error(&mut bootstrap, initial.revision).code,
            "WAKE_WATCH_COVERAGE_PENDING"
        );
        let widened = bootstrap.watch_plan();
        assert!(widened.revision > initial.revision);
        assert!(
            widened
                .interests
                .iter()
                .any(|interest| interest.matches_event(&refined, true))
        );
        let materialized = generation_seal_count(&fixture.0);
        assert_eq!(materialized, 1);
        assert_eq!(
            bootstrap_activation_error(&mut bootstrap, initial.revision).code,
            "WAKE_WATCH_COVERAGE_PENDING"
        );
        assert_eq!(generation_seal_count(&fixture.0), materialized);
        let context = bootstrap.activate_at(widened.revision).unwrap();
        assert_eq!(
            generation_seal_count(&fixture.0),
            materialized + 1,
            "covered activation must render a fresh immutable generation"
        );
        assert_eq!(
            bootstrap_activation_error(&mut bootstrap, widened.revision).code,
            "WAKE_INTERNAL"
        );
        assert_eq!(generation_seal_count(&fixture.0), materialized + 1);
        assert!(!fixture.0.join(".wake").exists());
        context.close();
    }

    #[test]
    fn build_watch_bootstrap_generated_failure_uses_an_exact_recovery_witness() {
        let fixture = Fixture::new("build-watch-bootstrap-generated-recovery");
        let fault = fixture.0.join(".wake/dev-candidates/fault");
        let error = WakeError::new("WAKE_IO", "candidate generation failed").at(&fault);
        let interests = build_watch_bootstrap_recovery_interests(
            &BuildOptions {
                project: fixture.project(),
                ..BuildOptions::default()
            },
            &fixture.0,
            &fixture.0,
            &error,
        );
        assert!(
            interests
                .iter()
                .any(|interest| interest.matches_event(&fault, true))
        );
        assert!(
            !interests
                .iter()
                .any(|interest| interest.matches_event(&fault.join("generated.js"), true))
        );
    }

    #[test]
    fn build_watch_bootstrap_preserves_outdir_until_the_first_success() {
        let fixture = Fixture::new("build-watch-bootstrap-output-sentinel");
        let output = fixture.0.join("dist");
        std::fs::create_dir_all(&output).unwrap();
        fixture.write("dist/sentinel.txt", "previous output\n");
        write_output_ownership_marker(&output, OutputProduct::Application).unwrap();
        let mut bootstrap = BuildWatchBootstrap::create(BuildOptions {
            project: fixture.project(),
            outdir: Some(output.clone()),
            write: true,
            ..BuildOptions::default()
        })
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output.join("sentinel.txt")).unwrap(),
            "previous output\n"
        );
        let context = activate_bootstrap_after_current_coverage(&mut bootstrap);
        assert_eq!(
            std::fs::read_to_string(output.join("sentinel.txt")).unwrap(),
            "previous output\n"
        );
        let plan = context.watch_plan();
        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    plan.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert!(!output.join("sentinel.txt").exists());
        assert!(output.join("index.html").is_file());
        context.close();
    }

    #[test]
    fn manual_build_context_creation_remains_eager() {
        let fixture = Fixture::new("manual-build-context-eager");
        assert!(!fixture.0.join(".wake").exists());
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        assert!(
            !fixture.0.join(".wake").exists(),
            "eager preparation must retain generated inputs in memory"
        );
        context.close();
    }

    #[test]
    fn candidate_generations_keep_stable_logical_entries_and_isolate_physical_inputs() {
        let fixture = Fixture::new("candidate-generation-isolation");
        let options = BuildOptions {
            project: fixture.project(),
            ..BuildOptions::default()
        };
        let first = prepare_build_candidate(&options).unwrap();
        let logical_entry = first.root.join(".wake/entry.tsx");
        assert_eq!(first.entry, logical_entry);
        let first_generation = first.generation.clone();
        let first_bytes = first_generation.file_system().read(&logical_entry).unwrap();
        assert_eq!(
            first
                .generation
                .file_system()
                .read_to_string(&logical_entry)
                .unwrap(),
            "import(\"@@/src/index.js\");\n"
        );

        fixture.write("src/other.js", "export const other = true;\n");
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/other.js\"\n",
        );
        let second = prepare_build_candidate(&options).unwrap();
        assert_eq!(second.entry, logical_entry);
        assert!(!first_generation.is_same_generation(&second.generation));
        assert_eq!(
            first_generation.file_system().read(&logical_entry).unwrap(),
            first_bytes
        );
        assert_eq!(
            second
                .generation
                .file_system()
                .read_to_string(&logical_entry)
                .unwrap(),
            "import(\"@@/src/other.js\");\n"
        );
        assert!(
            !fixture.0.join(".wake").exists(),
            "isolated generations must not create a physical candidate tree"
        );
    }

    #[test]
    fn generation_diff_reports_only_added_modified_and_removed_logical_files() {
        let fixture = Fixture::new("generation-logical-diff");
        let mut previous = GenerationDraft::new(&fixture.0);
        previous.write_file("same.js", b"same".as_slice()).unwrap();
        previous
            .write_file("modified.js", b"before".as_slice())
            .unwrap();
        previous
            .write_file("removed.js", b"removed".as_slice())
            .unwrap();
        let previous = previous.seal().unwrap();

        let mut next = GenerationDraft::new(&fixture.0);
        next.write_file("same.js", b"same".as_slice()).unwrap();
        next.write_file("modified.js", b"after".as_slice()).unwrap();
        next.write_file("added.js", b"added".as_slice()).unwrap();
        let next = next.seal().unwrap();

        assert_eq!(
            generation_changed_paths(&previous, &next),
            ["added.js", "modified.js", "removed.js"]
                .into_iter()
                .map(|path| fixture.0.join(".wake").join(path))
                .collect::<Vec<_>>()
        );
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn build_candidate_probe_declares_coverage_before_generation_materialization() {
        let fixture = Fixture::new("candidate-probe-before-materialize");
        fixture.write("packages/external.js", "export const external = true;\n");
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n[alias]\nexternal = \"packages/external.js\"\n",
        );
        let options = BuildOptions {
            project: fixture.project(),
            ..BuildOptions::default()
        };

        let probe = probe_build_candidate(&options).unwrap();
        assert!(
            !fixture.0.join(".wake").exists(),
            "a watch probe must not allocate a generation"
        );
        let interests = probe_watch_interests(&probe);
        assert!(
            interests
                .iter()
                .any(|interest| interest.matches(&fixture.0.join("packages/external.js"))),
            "candidate source coverage must be available before materialization"
        );

        let materialized = materialize_build_probe(probe, true).unwrap();
        assert!(
            materialized
                .generation
                .logical_inventory()
                .contains(&materialized.entry)
        );
        assert_eq!(
            materialized
                .generation
                .file_system()
                .read_to_string(&materialized.entry)
                .unwrap(),
            "import(\"@@/src/index.js\");\n"
        );
        assert!(
            !fixture.0.join(".wake").exists(),
            "materialization must remain an owned in-memory operation"
        );
    }

    #[test]
    fn docs_candidate_probe_does_not_generate_docs() {
        let fixture = Fixture::new("docs-candidate-probe");
        fixture.write("docs/index.md", "# Docs\n");
        let options = DocsBuildOptions {
            project: fixture.project(),
            ..DocsBuildOptions::default()
        };

        let probe = probe_docs_candidate(&options, DocsMode::Site).unwrap();
        assert!(!fixture.0.join(".wake").exists());
        assert!(
            docs_probe_watch_interests(&probe)
                .iter()
                .any(|interest| interest.matches(&fixture.0.join("docs/index.md")))
        );
    }

    #[test]
    fn control_discovery_tracks_nearest_ancestor_pnp_and_install_root_only() {
        let outer = tempfile::tempdir().unwrap();
        let install = outer.path().join("workspace");
        let root = install.join("packages/app");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(install.join(".pnp.cjs"), "module.exports = {};").unwrap();
        std::fs::write(install.join(".pnp.data.json"), "{}").unwrap();
        std::fs::write(install.join("yarn.lock"), "lock").unwrap();
        std::fs::write(outer.path().join("package-lock.json"), "unrelated").unwrap();

        let paths = project_control_paths(&root, &root, None);
        for expected in [
            install.join(".pnp.cjs"),
            install.join(".pnp.data.json"),
            install.join("yarn.lock"),
        ] {
            assert!(paths.contains(&expected), "missing {}", expected.display());
        }
        assert!(!paths.contains(&outer.path().join("package-lock.json")));
        assert!(
            !paths
                .iter()
                .any(|path| path.starts_with(root.join(".wake")))
        );
        if let Some(volume_root) = root.ancestors().last()
            && !volume_root.join(".pnp.cjs").is_file()
        {
            assert!(!paths.contains(&volume_root.join(".pnp.cjs")));
        }
    }

    #[test]
    fn control_discovery_follows_an_explicit_entry_outside_the_project() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("project");
        let external = outer.path().join("external-workspace");
        let entry = external.join("packages/app/index.js");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(external.join(".pnp.cjs"), "module.exports = {};").unwrap();
        std::fs::write(external.join(".pnp.data.json"), "{}").unwrap();
        std::fs::write(external.join("yarn.lock"), "lock").unwrap();

        let paths = project_control_paths(&root, &root, Some(&entry));
        assert!(paths.contains(&external.join(".pnp.cjs")));
        assert!(paths.contains(&external.join(".pnp.data.json")));
        assert!(paths.contains(&external.join("yarn.lock")));
    }

    #[test]
    fn materialized_build_keeps_the_probe_owned_control_snapshot() {
        let fixture = Fixture::new("probe-owned-control-snapshot");
        let options = BuildOptions {
            project: fixture.project(),
            ..BuildOptions::default()
        };
        let probe = probe_build_candidate(&options).unwrap();
        let probed_identity = build_probe_identity(&probe);

        fixture.write("package-lock.json", r#"{"lockfileVersion":3}"#);
        let prepared = materialize_build_probe(probe, true).unwrap();
        assert_eq!(
            build_probe_identity(&build_probe_from_prepared(&prepared)),
            probed_identity
        );
        assert_ne!(
            build_control_fingerprints(
                &prepared.config_dir,
                &prepared.root,
                prepared.explicit_entry.as_deref(),
            ),
            probed_identity.controls
        );
    }

    #[test]
    fn federation_lock_events_compare_content_and_allow_restoration() {
        let fixture = Fixture::new("federation-lock-fingerprint");
        let lock = fixture.0.join("wake-federation.lock");
        fixture.write("wake-federation.lock", "accepted");
        let accepted = control_file_fingerprint(&lock);
        let interest = WatchInterest::exact_file(&lock).resolve_against(&fixture.0);
        let changed = vec![lock.clone()];

        fixture.write("wake-federation.lock", "accepted");
        assert!(!federation_lock_changed(
            &changed, false, &interest, &lock, &accepted
        ));
        fixture.write("wake-federation.lock", "changed");
        assert!(federation_lock_changed(
            &changed, false, &interest, &lock, &accepted
        ));
        fixture.write("wake-federation.lock", "accepted");
        assert!(!federation_lock_changed(
            &[],
            true,
            &interest,
            &lock,
            &accepted
        ));
    }

    #[test]
    fn project_watch_plan_includes_controls_aliases_and_federation_exposes_outside_src() {
        let fixture = Fixture::new("typed-project-watch-plan");
        fixture.write(
            "packages/Button.tsx",
            "export default function Button() {}\n",
        );
        fixture.write(
            wake_config::CONFIG_FILE,
            r#"[html]
entry = "src/index.js"

[alias]
external = "packages/external.dotted"

[federation]
enabled = true
name = "shell"

[federation.exposes."./Button"]
entry = "packages/Button.tsx"
"#,
        );
        let prepared = prepare_build_candidate(&BuildOptions {
            project: fixture.project(),
            ..BuildOptions::default()
        })
        .unwrap();
        let interests = project_watch_interests(&prepared);
        let alias = fixture.0.join("packages/external.dotted");
        let expose = fixture.0.join("packages/Button.tsx");
        let config = fixture.0.join(wake_config::CONFIG_FILE);

        assert!(interests.iter().any(|interest| interest.matches(&alias)));
        assert!(interests.iter().any(|interest| interest.matches(&expose)));
        assert!(
            interests
                .iter()
                .any(|interest| interest.matches_exact_file(&config))
        );
        assert!(!interests.iter().any(|interest| {
            interest.matches_exact_file(&fixture.0.join("nested/wake.config.toml"))
        }));
    }

    #[test]
    fn build_rejects_project_root_and_source_output_without_touching_inputs() {
        for (label, outdir) in [("root-outdir", "."), ("source-outdir", "src")] {
            let fixture = Fixture::new(label);
            fixture.write("sentinel.txt", "keep-root");
            fixture.write("src/sentinel.txt", "keep-source");

            let error = build(
                BuildOptions {
                    project: fixture.project(),
                    outdir: Some(PathBuf::from(outdir)),
                    ..BuildOptions::default()
                },
                &CancellationToken::default(),
            )
            .unwrap_err();

            assert_eq!(error.code, "WAKE_CONFIG");
            assert_eq!(
                std::fs::read_to_string(fixture.0.join("sentinel.txt")).unwrap(),
                "keep-root"
            );
            assert_eq!(
                std::fs::read_to_string(fixture.0.join("src/sentinel.txt")).unwrap(),
                "keep-source"
            );
            assert!(fixture.0.join(wake_config::CONFIG_FILE).is_file());
        }
    }

    #[test]
    fn build_rejects_project_ancestor_output_without_touching_siblings() {
        let outer = tempfile::tempdir().unwrap();
        let project = outer.path().join("project");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join(wake_config::CONFIG_FILE),
            "[html]\nentry = \"src/index.js\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/index.js"), "export const value = 42;\n").unwrap();
        std::fs::write(outer.path().join("sibling.txt"), "keep-sibling").unwrap();

        let error = build(
            BuildOptions {
                project: ProjectOptions {
                    cwd: Some(project.clone()),
                    config_path: None,
                },
                outdir: Some(PathBuf::from("..")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(
            std::fs::read_to_string(outer.path().join("sibling.txt")).unwrap(),
            "keep-sibling"
        );
        assert!(project.join("src/index.js").is_file());
    }

    #[test]
    fn build_rejects_nonempty_unowned_output_directory() {
        let fixture = Fixture::new("unowned-output");
        fixture.write("custom-output/sentinel.txt", "keep-unowned");

        let error = build(
            BuildOptions {
                project: fixture.project(),
                outdir: Some(PathBuf::from("custom-output")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("custom-output/sentinel.txt")).unwrap(),
            "keep-unowned"
        );
    }

    fn output_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Vec<u8>> {
        collect_output_tree_files(root, "test snapshot", false)
            .unwrap()
            .into_iter()
            .map(|relative| {
                let bytes = std::fs::read(root.join(&relative)).unwrap();
                (relative, bytes)
            })
            .collect()
    }

    #[test]
    fn build_publishes_an_owned_tree_and_removes_stale_files() {
        let fixture = Fixture::new("owned-output");
        let options = BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist")),
            ..BuildOptions::default()
        };

        build(options.clone(), &CancellationToken::default()).unwrap();
        let output = fixture.0.join("dist");
        let ownership: OutputOwnership =
            serde_json::from_slice(&std::fs::read(output.join(OUTPUT_OWNERSHIP_FILE)).unwrap())
                .unwrap();
        assert_eq!(ownership, OutputOwnership::wake(OutputProduct::Application));
        std::fs::write(output.join("stale.txt"), "stale").unwrap();
        fixture.write("src/index.js", "export const value = 43;\n");

        build(options, &CancellationToken::default()).unwrap();

        assert!(!output.join("stale.txt").exists());
        assert!(output.join("index.html").is_file());
        assert!(output.join("manifest.json").is_file());
    }

    #[test]
    fn build_failure_preserves_the_last_published_tree() {
        let fixture = Fixture::new("publish-failure");
        let options = BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist")),
            ..BuildOptions::default()
        };
        build(options.clone(), &CancellationToken::default()).unwrap();
        let output = fixture.0.join("dist");
        let before = output_snapshot(&output);
        fixture.write("src/index.js", "export const = ;\n");

        let error = build(options, &CancellationToken::default()).unwrap_err();

        assert_eq!(error.code, "WAKE_BUILD");
        assert_eq!(output_snapshot(&output), before);
    }

    #[test]
    fn cancellation_is_visible_while_waiting_for_an_inflight_commit() {
        let cancellation = CancellationToken::default();
        let commit_cancellation = cancellation.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let commit = thread::spawn(move || {
            commit_cancellation.commit(|| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            })
        });
        entered_rx.recv().unwrap();

        let cancel_cancellation = cancellation.clone();
        let (cancelled_tx, cancelled_rx) = mpsc::channel();
        let cancel = thread::spawn(move || {
            cancel_cancellation.cancel();
            cancelled_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !cancellation.is_cancelled() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert!(
            cancellation.is_cancelled(),
            "cancellation must be published before waiting for the commit writer gate"
        );
        assert!(
            matches!(cancelled_rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "cancel should still be waiting for the in-flight commit"
        );

        release_tx.send(()).unwrap();
        commit.join().unwrap().unwrap();
        cancel.join().unwrap();
        cancelled_rx.recv().unwrap();
        assert_eq!(
            cancellation.commit(|| Ok(())).unwrap_err().code,
            "WAKE_CANCELLED"
        );
    }

    #[test]
    fn cancelled_exact_publication_never_reaches_the_destination() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("bundle.js");
        std::fs::write(&output, "last-good").unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let error = cancellation
            .commit(|| publish_exact_outputs(&[ExactOutput::write(&output, b"cancelled")], &[]))
            .unwrap_err();

        assert_eq!(error.code, "WAKE_CANCELLED");
        assert_eq!(std::fs::read_to_string(output).unwrap(), "last-good");
    }

    #[test]
    fn backend_cancellation_fences_staged_commit_until_recovery() {
        let fixture = Fixture::new("backend-cancelled-output-commit");
        let output = fixture.0.join("dist");
        std::fs::create_dir_all(&output).unwrap();
        write_output_ownership_marker(&output, OutputProduct::Application).unwrap();
        std::fs::write(output.join("sentinel.txt"), "accepted").unwrap();

        let cancellation = CancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let project = fixture.0.clone();
        let worker_output = output.clone();
        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            publish_staged_output(
                &project,
                &[],
                &worker_output,
                OutputProduct::Application,
                &worker_cancellation,
                |stage| {
                    std::fs::write(stage.join("index.html"), "stale")
                        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
                    staged_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        staged_rx.recv().unwrap();
        cancellation.cancel();
        release_tx.send(()).unwrap();
        let error = worker.join().unwrap().unwrap_err();
        assert_eq!(error.code, "WAKE_CANCELLED");
        assert_eq!(
            std::fs::read_to_string(output.join("sentinel.txt")).unwrap(),
            "accepted"
        );
        assert!(!output.join("index.html").exists());

        let recovered = CancellationToken::default();
        publish_staged_output(
            &fixture.0,
            &[],
            &output,
            OutputProduct::Application,
            &recovered,
            |stage| {
                std::fs::write(stage.join("index.html"), "recovered")
                    .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))
            },
        )
        .unwrap();
        assert!(!output.join("sentinel.txt").exists());
        assert_eq!(
            std::fs::read_to_string(output.join("index.html")).unwrap(),
            "recovered"
        );
    }

    #[test]
    fn build_rejects_output_owned_by_another_product() {
        let fixture = Fixture::new("product-mismatch");
        let output = fixture.0.join("dist");
        std::fs::create_dir_all(&output).unwrap();
        write_output_ownership_marker(&output, OutputProduct::Documentation).unwrap();
        std::fs::write(output.join("sentinel.txt"), "keep").unwrap();

        let error = build(
            BuildOptions {
                project: fixture.project(),
                outdir: Some(PathBuf::from("dist")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(
            std::fs::read_to_string(output.join("sentinel.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn build_allows_a_new_external_output_directory() {
        let outer = tempfile::tempdir().unwrap();
        let project = outer.path().join("project");
        let output = outer.path().join("published");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(
            project.join(wake_config::CONFIG_FILE),
            "[html]\nentry = \"src/index.js\"\n",
        )
        .unwrap();
        std::fs::write(project.join("src/index.js"), "export const value = 42;\n").unwrap();

        let result = build(
            BuildOptions {
                project: ProjectOptions {
                    cwd: Some(project),
                    config_path: None,
                },
                outdir: Some(output.clone()),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();

        assert_same_existing_file(result.output_dir.as_deref().unwrap(), &output);
        assert!(output.join(OUTPUT_OWNERSHIP_FILE).is_file());
    }

    #[test]
    fn staged_output_paths_reject_absolute_and_parent_traversal() {
        let stage = Path::new("stage");
        assert!(output_file_path(stage, "nested/file.js").is_ok());
        assert!(output_file_path(stage, "../escape.js").is_err());
        assert!(output_file_path(stage, Path::new("/absolute.js")).is_err());
        assert!(output_file_path(stage, "").is_err());
    }

    #[test]
    fn docs_leaf_publishes_routes_and_public_assets_in_one_owned_inventory() {
        let fixture = Fixture::new("docs-owned-output");
        fixture.write("docs/index.mdx", "+++\ntitle = \"Home\"\n+++\n\n# Home\n");
        fixture.write(
            "docs/navigation.toml",
            "[[group]]\nid = \"start\"\ntitle = \"Start\"\npages = [\"index\"]\n",
        );
        fixture.write(
            "package.json",
            r#"{"dependencies":{"react":"^19.2.8","react-dom":"^19.2.8"}}"#,
        );
        fixture.write(
            "node_modules/react/package.json",
            r#"{"name":"react","version":"19.2.8","type":"module","exports":{".":"./index.js","./jsx-runtime":"./jsx-runtime.js","./jsx-dev-runtime":"./jsx-runtime.js"}}"#,
        );
        fixture.write(
            "node_modules/react/index.js",
            "export default {}; export const Suspense = Symbol(); export const startTransition = f => f(); export const useCallback = f => f; export const useEffect = () => {}; export const useId = () => 'id'; export const useLayoutEffect = () => {}; export const useMemo = f => f(); export const useRef = v => ({ current: v }); export const useState = v => [v, () => {}];\n",
        );
        fixture.write(
            "node_modules/react/jsx-runtime.js",
            "export const Fragment = Symbol(); export const jsx = () => ({}); export const jsxs = jsx; export const jsxDEV = jsx;\n",
        );
        fixture.write(
            "node_modules/react-dom/package.json",
            r#"{"name":"react-dom","version":"19.2.8","type":"module","exports":{".":"./index.js","./client":"./client.js"}}"#,
        );
        fixture.write("node_modules/react-dom/index.js", "export default {};\n");
        fixture.write(
            "node_modules/react-dom/client.js",
            "export const createRoot = () => ({ render() {} });\n",
        );
        fixture.write("public/robots.txt", "User-agent: *\n");

        let result = build_docs(
            DocsBuildOptions {
                project: fixture.project(),
                outdir: Some(PathBuf::from("docs-dist")),
                ..DocsBuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();

        let output = fixture.0.join("docs-dist");
        let ownership: OutputOwnership =
            serde_json::from_slice(&std::fs::read(output.join(OUTPUT_OWNERSHIP_FILE)).unwrap())
                .unwrap();
        assert_eq!(
            ownership,
            OutputOwnership::wake(OutputProduct::Documentation)
        );
        let reported = result
            .build
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<BTreeSet<_>>();
        let actual = collect_output_tree_files(&output, "documentation", false)
            .unwrap()
            .into_iter()
            .filter(|path| path != Path::new(OUTPUT_OWNERSHIP_FILE))
            .map(|path| path.to_string_lossy().replace('\\', "/"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            reported,
            actual.iter().map(String::as_str).collect::<BTreeSet<_>>()
        );
        assert!(reported.contains("robots.txt"));
        assert!(reported.contains("index.html"));
    }

    #[cfg(unix)]
    #[test]
    fn output_symlink_is_rejected_without_touching_its_target() {
        use std::os::unix::fs::symlink;

        let fixture = Fixture::new("output-symlink");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel.txt"), "keep").unwrap();
        symlink(outside.path(), fixture.0.join("dist")).unwrap();

        let error = build(
            BuildOptions {
                project: fixture.project(),
                outdir: Some(PathBuf::from("dist")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
            "keep"
        );
    }

    #[cfg(windows)]
    #[test]
    fn output_reparse_point_is_rejected_without_touching_its_target() {
        use std::os::windows::fs::symlink_dir;

        let fixture = Fixture::new("output-reparse");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel.txt"), "keep").unwrap();
        if symlink_dir(outside.path(), fixture.0.join("dist")).is_err() {
            return;
        }

        let error = build(
            BuildOptions {
                project: fixture.project(),
                outdir: Some(PathBuf::from("dist")),
                ..BuildOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_CONFIG");
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel.txt")).unwrap(),
            "keep"
        );
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
    fn aggregated_lazy_docs_materialize_only_the_requested_workspace() {
        let directory = tempfile::Builder::new()
            .prefix("wake-app-aggregated-lazy-docs-")
            .tempdir_in(env!("CARGO_MANIFEST_DIR"))
            .unwrap();
        let root = directory.path();
        let write = |relative: &str, contents: &str| {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, contents).unwrap();
        };
        write(
            wake_config::CONFIG_FILE,
            r#"[docs]
base_path = "/docs/"

[[docs.workspace]]
root = "components"
include = ["rc-*"]
base_path = "/components/{name}/workbench/"
dev_loading = "lazy"
"#,
        );
        write("docs/index.mdx", "# Site\n");
        write(
            "docs/navigation.toml",
            "[[group]]\nid = \"main\"\ntitle = \"Main\"\npages = [\"index\"]\n",
        );
        write(
            "package.json",
            r#"{"dependencies":{"react":"19.2.8","react-dom":"19.2.8"}}"#,
        );
        for name in ["rc-alpha", "rc-beta"] {
            write(&format!("components/{name}/wake.config.toml"), "");
            write(
                &format!("components/{name}/docs/index.mdx"),
                "# Component\n",
            );
            write(
                &format!("components/{name}/docs/navigation.toml"),
                "[[group]]\nid = \"main\"\ntitle = \"Main\"\npages = [\"index\"]\n",
            );
            write(
                &format!("components/{name}/package.json"),
                r#"{"dependencies":{"react":"19.2.8","react-dom":"19.2.8"}}"#,
            );
        }
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let server = start_docs_dev_server(DevServerOptions {
            project: ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            port: Some(port),
            open: Some(false),
            ..DevServerOptions::default()
        })
        .unwrap();
        let alpha = root.join("components/rc-alpha");
        let beta = root.join("components/rc-beta");
        assert_eq!(generation_seal_count(&alpha), 0);
        assert_eq!(generation_seal_count(&beta), 0);

        let response = http_get(port, "/docs/components/rc-alpha/workbench/bundle.js");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(generation_seal_count(&alpha), 1);
        assert_eq!(generation_seal_count(&beta), 0);
        assert!(!alpha.join(".wake").exists());
        assert!(!beta.join(".wake").exists());
        server.close().unwrap();
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

    #[test]
    fn directory_output_rejects_a_staged_commit_lock_file() {
        let fixture = Fixture::new("directory-output-reserved-lock");
        let staging = fixture.0.join("stage");
        let target = fixture.0.join("docs-dist");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(staging.join(".wake-output.lock"), "artifact").unwrap();
        std::fs::write(target.join("index.html"), "accepted").unwrap();

        let error = commit_output_tree(&staging, &target).unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(
            std::fs::read_to_string(target.join("index.html")).unwrap(),
            "accepted"
        );
        assert!(!target.join(".wake-output.lock").exists());
    }

    #[test]
    fn directory_output_preserves_existing_reserved_lock_metadata() {
        let fixture = Fixture::new("directory-output-preserves-reserved-lock");
        let staging = fixture.0.join("stage");
        let target = fixture.0.join("docs-dist");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(staging.join("index.html"), "new").unwrap();
        let lock_path = target.join(".wake-output.lock");
        std::fs::write(&lock_path, "reserved").unwrap();
        std::fs::write(target.join("stale.txt"), "stale").unwrap();
        let original = same_file::Handle::from_path(&lock_path).unwrap();

        commit_output_tree(&staging, &target).unwrap();

        assert_eq!(same_file::Handle::from_path(&lock_path).unwrap(), original);
        assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), "reserved");
        assert!(!target.join("stale.txt").exists());
    }

    #[test]
    fn directory_output_lock_serializes_injected_rollback_before_the_next_commit() {
        let fixture = Fixture::new("directory-output-lock-rollback");
        let target = fixture.0.join("docs-dist");
        let failed_staging = fixture.0.join("failed-stage");
        let successful_staging = fixture.0.join("successful-stage");
        for directory in [&target, &failed_staging, &successful_staging] {
            std::fs::create_dir_all(directory).unwrap();
        }
        for (directory, generation) in [
            (&target, "old"),
            (&failed_staging, "failed"),
            (&successful_staging, "accepted"),
        ] {
            std::fs::write(directory.join("a.txt"), format!("{generation}-a")).unwrap();
            std::fs::write(directory.join("b.txt"), format!("{generation}-b")).unwrap();
        }

        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let failed_target = target.clone();
        let failed_writer = thread::spawn(move || {
            commit_staged_output_with(
                &failed_staging,
                &failed_target,
                None,
                "documentation",
                ".wake-docs-backup-",
                || {
                    locked_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                Some(1),
            )
        });
        locked_rx.recv().unwrap();

        let (completed_tx, completed_rx) = mpsc::channel();
        let successful_target = target.clone();
        let successful_writer = thread::spawn(move || {
            let result = commit_output_tree(&successful_staging, &successful_target);
            completed_tx.send(()).unwrap();
            result
        });
        assert!(
            matches!(
                completed_rx.recv_timeout(Duration::from_millis(200)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a writer for the same target entered while rollback still owned the target lock"
        );

        release_tx.send(()).unwrap();
        let failure = failed_writer.join().unwrap().unwrap_err();
        assert_eq!(failure.code, "WAKE_IO");
        assert!(
            failure
                .message
                .contains("injected directory output install failure")
        );
        successful_writer.join().unwrap().unwrap();
        completed_rx.recv().unwrap();

        assert_eq!(
            std::fs::read_to_string(target.join("a.txt")).unwrap(),
            "accepted-a"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("b.txt")).unwrap(),
            "accepted-b"
        );
    }

    #[test]
    fn nested_directory_outputs_share_one_commit_lock() {
        let fixture = Fixture::new("nested-directory-output-lock");
        let parent_target = fixture.0.join("dist");
        let child_target = parent_target.join("nested");
        let parent_staging = fixture.0.join("parent-stage");
        let child_staging = fixture.0.join("child-stage");
        for directory in [
            &parent_target,
            &child_target,
            &parent_staging,
            &child_staging,
        ] {
            std::fs::create_dir_all(directory).unwrap();
        }
        std::fs::write(child_target.join("old.txt"), "old").unwrap();
        std::fs::write(parent_staging.join("parent.txt"), "parent").unwrap();
        std::fs::write(child_staging.join("child.txt"), "child").unwrap();

        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let parent_writer = thread::spawn(move || {
            commit_staged_output_with(
                &parent_staging,
                &parent_target,
                None,
                "documentation",
                ".wake-docs-backup-",
                || {
                    locked_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                None,
            )
        });
        locked_rx.recv().unwrap();

        let (completed_tx, completed_rx) = mpsc::channel();
        let child_writer = thread::spawn(move || {
            let result = commit_output_tree(&child_staging, &child_target);
            completed_tx.send(()).unwrap();
            result
        });
        assert!(
            matches!(
                completed_rx.recv_timeout(Duration::from_millis(200)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a nested directory publisher bypassed the parent publisher's commit lock"
        );

        release_tx.send(()).unwrap();
        parent_writer.join().unwrap().unwrap();
        child_writer.join().unwrap().unwrap();
        completed_rx.recv().unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("dist/parent.txt")).unwrap(),
            "parent"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("dist/nested/child.txt")).unwrap(),
            "child"
        );
    }

    #[test]
    fn exact_staging_inside_a_directory_target_is_locked_before_creation() {
        let fixture = Fixture::new("exact-inside-directory-staging-lock");
        let target = fixture.0.join("dist");
        let parent_staging = fixture.0.join("parent-stage");
        let exact_path = target.join("bundle.js");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&parent_staging).unwrap();
        std::fs::write(parent_staging.join("bundle.js"), "parent").unwrap();

        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let exact_writer = thread::spawn(move || {
            output::publish_exact_outputs_with_staging_hook(
                &[ExactOutput::write(&exact_path, b"exact")],
                &[],
                || {
                    staged_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
            )
        });
        staged_rx.recv().unwrap();

        let (completed_tx, completed_rx) = mpsc::channel();
        let parent_writer = thread::spawn(move || {
            let result = commit_output_tree(&parent_staging, &target);
            completed_tx.send(()).unwrap();
            result
        });
        assert!(
            matches!(
                completed_rx.recv_timeout(Duration::from_millis(200)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "an ancestor directory publisher reached commit while exact staging was live"
        );

        release_tx.send(()).unwrap();
        exact_writer.join().unwrap().unwrap();
        parent_writer.join().unwrap().unwrap();
        completed_rx.recv().unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("dist/bundle.js")).unwrap(),
            "parent"
        );
    }

    #[test]
    fn child_directory_materialization_stays_outside_the_parent_output_tree() {
        let fixture = Fixture::new("child-directory-safe-staging-domain");
        let project_root = fixture.0.clone();
        let parent_target = fixture.0.join("dist");
        let child_target = parent_target.join("child");
        std::fs::create_dir_all(&parent_target).unwrap();
        write_output_ownership_marker(&parent_target, OutputProduct::Application).unwrap();
        std::fs::write(parent_target.join("old.txt"), "old").unwrap();

        let (staged_tx, staged_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let child_root = project_root.clone();
        let child_writer = thread::spawn(move || {
            publish_staged_output(
                &child_root,
                &[],
                &child_target,
                OutputProduct::Application,
                &CancellationToken::default(),
                |stage| {
                    std::fs::write(stage.join("child.txt"), "child").unwrap();
                    staged_tx.send(stage.to_path_buf()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
            )
        });
        let child_stage = staged_rx.recv().unwrap();
        let stage_was_safe = !child_stage.starts_with(&parent_target);

        let parent_result = publish_staged_output(
            &project_root,
            &[],
            &parent_target,
            OutputProduct::Application,
            &CancellationToken::default(),
            |stage| {
                std::fs::write(stage.join("parent.txt"), "parent").unwrap();
                Ok(())
            },
        );
        release_tx.send(()).unwrap();

        assert!(
            stage_was_safe,
            "child staging was materialized inside an ancestor output's cleanup scope"
        );
        parent_result.unwrap();
        child_writer.join().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(parent_target.join("parent.txt")).unwrap(),
            "parent"
        );
        assert_eq!(
            std::fs::read_to_string(parent_target.join("child/child.txt")).unwrap(),
            "child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn directory_output_rejects_a_scope_containing_a_live_commit_lock() {
        let commit = acquire_output_commit_lock("directory lock collision test").unwrap();
        let scope = commit.lock_paths()[0]
            .parent()
            .expect("global Unix lock has a parent")
            .to_path_buf();

        let error = validate_directory_output_commit_scope(
            std::slice::from_ref(&scope),
            commit.lock_paths(),
            "documentation",
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
    }

    #[test]
    fn output_commit_lock_blocks_directory_and_exact_publishers_in_a_separate_process() {
        const READY_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_READY";
        const RELEASE_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_RELEASE";

        let fixture = Fixture::new("directory-output-process-lock");
        let target = fixture.0.join("docs-dist");
        let staging = fixture.0.join("stage");
        let exact = fixture.0.join("bundle.js");
        let ready = fixture.0.join("lock-ready");
        let release = fixture.0.join("lock-release");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(target.join("index.html"), "old").unwrap();
        std::fs::write(staging.join("index.html"), "new").unwrap();

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "tests::directory_output_lock_process_helper",
            ])
            .env(READY_ENV, &ready)
            .env(RELEASE_ENV, &release)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                panic!("output lock helper exited before acquiring its lock: {status}");
            }
            thread::sleep(Duration::from_millis(10));
        }
        if !ready.is_file() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("output lock helper did not acquire its lock");
        }

        let (directory_completed_tx, directory_completed_rx) = mpsc::channel();
        let directory_writer = thread::spawn(move || {
            let result = commit_output_tree(&staging, &target);
            directory_completed_tx.send(()).unwrap();
            result
        });
        let (exact_completed_tx, exact_completed_rx) = mpsc::channel();
        let exact_writer = thread::spawn(move || {
            let result = publish_exact_outputs(&[ExactOutput::write(&exact, b"exact")], &[]);
            exact_completed_tx.send(()).unwrap();
            result
        });
        let directory_blocked = matches!(
            directory_completed_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        let exact_blocked = matches!(
            exact_completed_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
        std::fs::write(&release, "release").unwrap();
        let helper_status = child.wait().unwrap();
        let directory_result = directory_writer.join().unwrap();
        let exact_result = exact_writer.join().unwrap();

        assert!(helper_status.success(), "output lock helper failed");
        assert!(
            directory_blocked,
            "a directory publisher bypassed the separate process commit lock"
        );
        assert!(
            exact_blocked,
            "an exact publisher bypassed the separate process commit lock"
        );
        directory_result.unwrap();
        exact_result.unwrap();
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("docs-dist/index.html")).unwrap(),
            "new"
        );
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("bundle.js")).unwrap(),
            "exact"
        );
    }

    #[test]
    fn output_commit_lock_recovers_after_a_holder_process_exits() {
        const READY_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_READY";
        const RELEASE_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_RELEASE";
        const ABANDON_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_ABANDON";

        let fixture = Fixture::new("output-process-lock-abandonment");
        let ready = fixture.0.join("lock-ready");
        let release = fixture.0.join("unused-release");
        let output = fixture.0.join("bundle.js");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "tests::directory_output_lock_process_helper",
            ])
            .env(READY_ENV, &ready)
            .env(RELEASE_ENV, &release)
            .env(ABANDON_ENV, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(
                    !status.success(),
                    "abandonment helper unexpectedly succeeded"
                );
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready.is_file(),
            "abandonment helper never acquired the lock"
        );
        let status = child.wait().unwrap();
        assert!(
            !status.success(),
            "abandonment helper unexpectedly succeeded"
        );

        publish_exact_outputs(&[ExactOutput::write(&output, b"recovered")], &[]).unwrap();
        assert_eq!(std::fs::read_to_string(output).unwrap(), "recovered");
    }

    #[test]
    #[ignore = "invoked as a child process by the output commit lock regression"]
    fn directory_output_lock_process_helper() {
        const READY_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_READY";
        const RELEASE_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_RELEASE";
        const ABANDON_ENV: &str = "WAKE_TEST_OUTPUT_LOCK_ABANDON";
        if std::env::var_os(READY_ENV).is_none() {
            return;
        }
        let ready = PathBuf::from(std::env::var_os(READY_ENV).expect("missing ready path"));
        let release = PathBuf::from(std::env::var_os(RELEASE_ENV).expect("missing release path"));
        let _lock = acquire_output_commit_lock("test helper").unwrap();
        std::fs::write(ready, "ready").unwrap();
        if std::env::var_os(ABANDON_ENV).is_some() {
            std::process::exit(86);
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while !release.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            release.is_file(),
            "parent did not release output lock helper"
        );
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
        assert!(
            result
                .files
                .iter()
                .any(|file| file.kind == OutputFileKind::Html)
        );

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
    fn build_context_rescan_recovers_a_sticky_configuration_error() {
        let fixture = Fixture::new("build-context-rescan-recovery");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        assert!(
            context
                .rebuild_watch(WatchInvalidation::Rescan, CancellationToken::default())
                .unwrap()
                .success
        );

        let config = fixture.0.join(wake_config::CONFIG_FILE);
        fixture.write(wake_config::CONFIG_FILE, "[html\n");
        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![config.clone()]),
                    CancellationToken::default(),
                )
                .is_err()
        );
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n",
        );
        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![fixture.0.join("src/index.js")]),
                    CancellationToken::default(),
                )
                .is_err(),
            "ordinary source events must not erase a sticky control failure"
        );
        assert!(
            context
                .rebuild_watch(WatchInvalidation::Rescan, CancellationToken::default())
                .unwrap()
                .success
        );
        context.close();
    }

    #[test]
    fn build_context_watch_revision_widens_before_a_failed_candidate_can_recover() {
        let fixture = Fixture::new("build-context-watch-widening");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        context
            .rebuild_watch(WatchInvalidation::Rescan, CancellationToken::default())
            .unwrap();
        let initial = context.watch_plan();
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n\n[alias]\nmissing = \"packages/missing.js\"\n",
        );
        fixture.write(
            "src/index.js",
            "import { value } from \"missing\"; export { value };\n",
        );

        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![fixture.0.join(wake_config::CONFIG_FILE)]),
                    CancellationToken::default(),
                )
                .is_err()
        );
        let widened = context.watch_plan();
        let missing = fixture.0.join("packages/missing.js");
        assert!(widened.revision > initial.revision);
        assert!(
            widened
                .interests
                .iter()
                .any(|interest| interest.matches_event(&missing, true))
        );

        fixture.write("packages/missing.js", "export const value = 7;\n");
        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![missing]),
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        context.close();
    }

    #[test]
    fn build_context_revision_capability_fences_materialization() {
        let fixture = Fixture::new("build-context-revision-capability");
        fixture.write("packages/external.js", "export const external = true;\n");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let initial = context.watch_plan();
        let initial_generations = generation_seal_count(&fixture.0);
        fixture.write("src/index.js", "export const temporarilyBroken = ;\n");
        for invalidation in [
            WatchInvalidation::Paths(vec![fixture.0.join("src/index.js")]),
            WatchInvalidation::Rescan,
        ] {
            assert_eq!(
                context
                    .rebuild_watch_at(
                        invalidation,
                        WatchPlanRevision(initial.revision.0 + 1),
                        CancellationToken::default(),
                    )
                    .unwrap_err()
                    .code,
                "WAKE_WATCH_COVERAGE_PENDING"
            );
        }
        assert_eq!(generation_seal_count(&fixture.0), initial_generations);
        fixture.write("src/index.js", "export const value = 42;\n");
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n[alias]\nexternal = \"packages/external.js\"\n",
        );

        let error = context
            .rebuild_watch_at(
                WatchInvalidation::Paths(vec![fixture.0.join(wake_config::CONFIG_FILE)]),
                initial.revision,
                CancellationToken::default(),
            )
            .unwrap_err();
        assert_eq!(error.code, "WAKE_WATCH_COVERAGE_PENDING");
        assert_eq!(generation_seal_count(&fixture.0), initial_generations);
        let widened = context.watch_plan();
        assert!(widened.revision > initial.revision);

        for stale in [initial.revision, WatchPlanRevision(widened.revision.0 + 1)] {
            assert_eq!(
                context
                    .rebuild_watch_at(
                        WatchInvalidation::Rescan,
                        stale,
                        CancellationToken::default(),
                    )
                    .unwrap_err()
                    .code,
                "WAKE_WATCH_COVERAGE_PENDING"
            );
            assert_eq!(generation_seal_count(&fixture.0), initial_generations);
        }

        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    widened.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert!(generation_seal_count(&fixture.0) > initial_generations);
        assert!(!fixture.0.join(".wake").exists());
        context.close();
    }

    #[test]
    fn build_context_rescan_rematerializes_component_scan_inputs() {
        let fixture = Fixture::new("build-context-component-scan-rescan");
        fixture.write("pages/a.ts", "export const page = 'before';\n");
        fixture.write(
            wake_config::CONFIG_FILE,
            r#"[html]
entry = "src/index.js"

[[component_scan]]
namespace = "pages"
cwd = "pages"
generate_source = true
"#,
        );
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let plan = context.watch_plan();
        let before = generation_seal_count(&fixture.0);
        fixture.write("pages/a.ts", "export const page = 'after';\n");

        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    plan.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert!(generation_seal_count(&fixture.0) > before);
        assert!(!fixture.0.join(".wake").exists());
        context.close();
    }

    #[test]
    fn build_context_same_snapshot_rescan_reuses_accepted_generation() {
        let fixture = Fixture::new("build-context-same-snapshot");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let plan = context.watch_plan();
        let before = generation_seal_count(&fixture.0);

        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    plan.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert_eq!(generation_seal_count(&fixture.0), before);
        context.close();
    }

    #[test]
    fn build_context_control_fingerprint_change_replaces_the_session() {
        let fixture = Fixture::new("build-context-control-fingerprint");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let plan = context.watch_plan();
        let before = generation_seal_count(&fixture.0);
        let browsers = fixture.0.join(".browserslistrc");
        fixture.write(".browserslistrc", "chrome 120\n");

        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Paths(vec![browsers]),
                    plan.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert!(generation_seal_count(&fixture.0) > before);
        assert!(!fixture.0.join(".wake").exists());
        context.close();
    }

    #[test]
    fn build_context_federation_lock_blocks_only_real_content_drift_and_recovers() {
        let fixture = Fixture::new("build-context-federation-lock-content");
        fixture.write("wake-federation.lock", "accepted");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let lock = fixture.0.join("wake-federation.lock");

        fixture.write("wake-federation.lock", "accepted");
        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![lock.clone()]),
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );

        fixture.write("wake-federation.lock", "changed");
        assert_eq!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![lock.clone()]),
                    CancellationToken::default(),
                )
                .unwrap_err()
                .code,
            "WAKE_DEV_RESTART_REQUIRED"
        );

        fixture.write("wake-federation.lock", "accepted");
        assert!(
            context
                .rebuild_watch(
                    WatchInvalidation::Paths(vec![lock]),
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        context.close();
    }

    #[test]
    fn build_context_failed_candidate_reuses_its_materialized_generation() {
        let fixture = Fixture::new("build-context-candidate-retry");
        fixture.write("packages/external.js", "export const external = true;\n");
        let context = BuildContext::create(BuildOptions {
            project: fixture.project(),
            write: false,
            ..BuildOptions::default()
        })
        .unwrap();
        let initial_seals = generation_seal_count(&fixture.0);
        let initial = context.watch_plan();
        fixture.write(
            wake_config::CONFIG_FILE,
            "[html]\nentry = \"src/index.js\"\n[alias]\nexternal = \"packages/external.js\"\n",
        );
        fixture.write("src/index.js", "export const broken = ;\n");
        assert_eq!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Paths(vec![fixture.0.join(wake_config::CONFIG_FILE)]),
                    initial.revision,
                    CancellationToken::default(),
                )
                .unwrap_err()
                .code,
            "WAKE_WATCH_COVERAGE_PENDING"
        );
        let widened = context.watch_plan();
        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Rescan,
                    widened.revision,
                    CancellationToken::default(),
                )
                .is_err()
        );
        let failed_generation = generation_seal_count(&fixture.0);
        assert_eq!(failed_generation, initial_seals + 1);

        let source = fixture.0.join("src/index.js");
        fixture.write("src/index.js", "export const fixed = true;\n");
        assert!(
            context
                .rebuild_watch_at(
                    WatchInvalidation::Paths(vec![source]),
                    widened.revision,
                    CancellationToken::default(),
                )
                .unwrap()
                .success
        );
        assert_eq!(generation_seal_count(&fixture.0), failed_generation);
        assert!(!fixture.0.join(".wake").exists());
        context.close();
    }

    #[test]
    fn write_build_watch_plan_excludes_owned_output_from_root_fallback() {
        let fixture = Fixture::new("build-context-output-exclusion");
        std::fs::remove_dir_all(fixture.0.join("src")).unwrap();
        fixture.write("index.js", "export const value = 42;\n");
        fixture.write(wake_config::CONFIG_FILE, "[html]\nentry = \"index.js\"\n");
        let options = BuildOptions {
            project: fixture.project(),
            outdir: Some(PathBuf::from("dist")),
            write: true,
            ..BuildOptions::default()
        };
        let prepared = prepare_build_candidate(&options).unwrap();
        let interests = build_context_watch_interests(&prepared, &options);

        assert!(
            !interests
                .iter()
                .any(|interest| { interest.matches_event(&fixture.0.join("dist/chunk.js"), true) })
        );
        assert!(
            interests
                .iter()
                .any(|interest| { interest.matches(&fixture.0.join("index.js")) })
        );
    }

    #[test]
    fn watch_plan_revision_changes_only_when_the_snapshot_value_changes() {
        let root = PathBuf::from("project");
        let initial = WatchInterest::tree(root.join("src"));
        let plan = RwLock::new(WatchPlanSnapshot {
            revision: WatchPlanRevision(0),
            root: root.clone(),
            interests: vec![initial.clone()],
        });
        replace_build_watch_plan(&plan, root.clone(), vec![initial.clone()]);
        assert_eq!(plan.read().unwrap().revision, WatchPlanRevision(0));

        replace_build_watch_plan(
            &plan,
            root.clone(),
            vec![initial.clone(), WatchInterest::tree(root.join("packages"))],
        );
        assert_eq!(plan.read().unwrap().revision, WatchPlanRevision(1));
        replace_build_watch_plan(
            &plan,
            root,
            vec![WatchInterest::tree("project/packages"), initial],
        );
        assert_eq!(plan.read().unwrap().revision, WatchPlanRevision(1));
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
        assert!(
            memory
                .files
                .iter()
                .any(|file| file.kind == OutputFileKind::SourceMap)
        );

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
    fn bundle_rejects_exact_output_over_entry_or_transitive_input() {
        let fixture = Fixture::new("bundle-input-collision");
        fixture.write(
            "src/index.js",
            "import { value } from './dep.js'; export { value };\n",
        );
        fixture.write("src/dep.js", "export const value = 42;\n");
        fixture.write("sentinel.txt", "outside");

        for outfile in ["src/index.js", "src/dep.js"] {
            let before = std::fs::read(fixture.0.join(outfile)).unwrap();
            let error = bundle(
                BundleOptions {
                    project: fixture.project(),
                    entry: Some(PathBuf::from("src/index.js")),
                    outfile: Some(PathBuf::from(outfile)),
                    ..BundleOptions::default()
                },
                &CancellationToken::default(),
            )
            .unwrap_err();

            assert_eq!(error.code, "WAKE_OUTPUT_COLLISION", "{outfile}");
            assert_eq!(std::fs::read(fixture.0.join(outfile)).unwrap(), before);
            assert_eq!(
                std::fs::read_to_string(fixture.0.join("sentinel.txt")).unwrap(),
                "outside"
            );
        }
    }

    #[test]
    fn bundle_rejects_output_inside_the_owned_generation_namespace() {
        let fixture = Fixture::new("bundle-owned-generation-output");
        let error = bundle(
            BundleOptions {
                project: fixture.project(),
                outfile: Some(PathBuf::from(".wake/entry.tsx")),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_CONFIG");
        assert!(error.message.contains("reserved `.wake`"));
        assert!(!fixture.0.join(".wake").exists());
    }

    #[test]
    fn bundle_rejects_exact_output_over_the_loaded_project_configuration() {
        let fixture = Fixture::new("bundle-config-collision");
        fixture.write("sentinel.txt", "outside");
        let config = fixture.0.join(wake_config::CONFIG_FILE);
        let before = std::fs::read(&config).unwrap();

        let error = bundle(
            BundleOptions {
                project: fixture.project(),
                outfile: Some(PathBuf::from(wake_config::CONFIG_FILE)),
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(std::fs::read(config).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("sentinel.txt")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn bundle_rejects_source_map_companion_over_a_read_input() {
        let fixture = Fixture::new("bundle-map-input-collision");
        fixture.write(
            "artifacts/result.js.map",
            "export const sourceValue = 42;\n",
        );
        fixture.write("sentinel.txt", "outside");
        let before = std::fs::read(fixture.0.join("artifacts/result.js.map")).unwrap();

        let error = bundle(
            BundleOptions {
                project: fixture.project(),
                entry: Some(PathBuf::from("artifacts/result.js.map")),
                outfile: Some(PathBuf::from("artifacts/result.js")),
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(
            std::fs::read(fixture.0.join("artifacts/result.js.map")).unwrap(),
            before
        );
        assert!(!fixture.0.join("artifacts/result.js").exists());
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("sentinel.txt")).unwrap(),
            "outside"
        );
    }

    #[test]
    fn bundle_replaces_code_but_preserves_an_unowned_stale_map() {
        let fixture = Fixture::new("bundle-stale-map");
        let outfile = fixture.0.join("artifacts/extension.js");
        let mapfile = fixture.0.join("artifacts/extension.js.map");
        let with_map = bundle(
            BundleOptions {
                project: fixture.project(),
                outfile: Some(PathBuf::from("artifacts/extension.js")),
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();
        assert!(mapfile.is_file());
        assert_eq!(with_map.files.len(), 2);
        let previous_map = std::fs::read(&mapfile).unwrap();

        fixture.write("src/index.js", "export const value = 84;\n");
        let without_map = bundle(
            BundleOptions {
                project: fixture.project(),
                outfile: Some(PathBuf::from("artifacts/extension.js")),
                source_map: false,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap();

        assert!(mapfile.exists());
        assert_eq!(std::fs::read(&mapfile).unwrap(), previous_map);
        assert_eq!(std::fs::read_to_string(&outfile).unwrap(), without_map.code);
        assert_eq!(without_map.files.len(), 1);
        assert_same_existing_file(&without_map.files[0].path, &outfile);
        assert_eq!(
            without_map.files[0].bytes,
            std::fs::read(&outfile).unwrap().len()
        );
        assert!(without_map.source_map.is_none());
        assert!(without_map.source_map_file.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn bundle_locked_map_failure_preserves_the_previous_code_map_pair() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let fixture = Fixture::new("bundle-locked-map");
        let outfile = fixture.0.join("artifacts/extension.js");
        let mapfile = fixture.0.join("artifacts/extension.js.map");
        std::fs::create_dir_all(outfile.parent().unwrap()).unwrap();
        std::fs::write(&outfile, "old-code").unwrap();
        std::fs::write(&mapfile, "old-map").unwrap();
        fixture.write("sentinel.txt", "outside");
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&mapfile)
            .unwrap();

        let error = bundle(
            BundleOptions {
                project: fixture.project(),
                outfile: Some(PathBuf::from("artifacts/extension.js")),
                source_map: true,
                ..BundleOptions::default()
            },
            &CancellationToken::default(),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_IO");
        assert_eq!(std::fs::read_to_string(outfile).unwrap(), "old-code");
        assert_eq!(std::fs::read_to_string(mapfile).unwrap(), "old-map");
        assert_eq!(
            std::fs::read_to_string(fixture.0.join("sentinel.txt")).unwrap(),
            "outside"
        );
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
