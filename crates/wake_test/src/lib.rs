//! Wake's authoritative JavaScript test runner.
//!
//! Test modules are preprocessed by Wake and executed in one isolated ECMAScript realm per suite.
//! The embedded JavaScript runtime is a private implementation resource; callers only exchange the
//! owned models in this module.

mod coverage_threshold;
mod git_changed;
mod selection;

pub use wake_test_contract::protocol;
pub use wake_test_contract::{
    BrowserEnvironmentInfo, CoverageFile, CoverageMetric, CoverageMetrics, CoverageResult,
    DiagnosticSeverity, SnapshotSummary, TestArtifact, TestCaseResult, TestCaseStatusCounts,
    TestDiagnostic, TestDiff, TestEnvironmentInfo, TestFailure, TestLeak, TestLocation,
    TestOptions, TestRunCounts, TestRunResult, TestStatus, TestStatusCounts, TestSuiteResult,
    TestTerminationReason, WorkerOverride,
};

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use base64::Engine;
use regex::Regex;
use serde::{Deserialize, Serialize};
use wake_common::SourceFile;
use wake_js_runtime::{
    CompiledCommonJsGraphScript, CompiledCommonJsModuleGraph, JsRuntime, RuntimeError,
    compile_commonjs_module_graph, emit_commonjs_graph_script,
};
use wake_test_browser::{
    BrowserCancellationToken, BrowserDriver, BrowserError, BrowserKind, BrowserLaunchOptions,
    BrowserPage, CdpEventWait, FetchFulfillResponse, FetchHeader, FetchRequestPaused,
    InputModifiers, KeyboardInput, PointerButton, PointerEventType, PointerInput, REDUCED_MOTION,
    ScreenshotClip, Viewport,
};

use selection::{RelatedOrigin, SuiteGraphIndex, SuiteIdentity};

const WAKE_TEST_RUNTIME: &str = include_str!("../runtime/wake-test-runtime.js");
const BROWSER_HOST_PRELUDE: &str = r#"
globalThis.IS_REACT_ACT_ENVIRONMENT = true
globalThis.__wakeHostCall = request => {
  const {op} = JSON.parse(String(request))
  const value = op === 'env' ? {}
    : op === 'cwd' ? '/'
    : op === 'execPath' ? 'wake-browser'
    : op === 'platform' ? String(navigator.platform || 'browser').toLowerCase()
    : (() => { throw Object.assign(new Error(`Browser host operation ${op} is unsupported`), {code: 'WAKE_TEST_UNSUPPORTED'}) })()
  return JSON.stringify(value)
}
"#;

#[derive(Debug, Default)]
struct TestCancellationState {
    cancelled: AtomicBool,
    gate: Mutex<()>,
    wake: Condvar,
}

/// Cross-thread cancellation seam owned by Wake Test. This is an internal Rust integration type;
/// JavaScript callers cancel through `AbortSignal` and the persistent test-host protocol.
#[derive(Debug, Clone, Default)]
pub struct TestCancellationToken(Arc<TestCancellationState>);

impl TestCancellationToken {
    pub fn cancel(&self) -> bool {
        let changed = !self.0.cancelled.swap(true, Ordering::AcqRel);
        self.0.wake.notify_all();
        changed
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    fn wait_for_cancellation_or_completion(&self, completed: &AtomicBool) -> bool {
        let mut guard = self
            .0
            .gate
            .lock()
            .expect("test cancellation lock is not poisoned");
        while !self.is_cancelled() && !completed.load(Ordering::Acquire) {
            guard = self
                .0
                .wake
                .wait(guard)
                .expect("test cancellation lock is not poisoned");
        }
        self.is_cancelled()
    }

    fn notify_waiters(&self) {
        self.0.wake.notify_all();
    }
}

struct RuntimeCancellationGuard {
    completed: Arc<AtomicBool>,
    cancellation: TestCancellationToken,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl RuntimeCancellationGuard {
    fn arm(runtime: &mut JsRuntime, cancellation: &TestCancellationToken) -> Self {
        let completed = Arc::new(AtomicBool::new(false));
        let worker_completed = Arc::clone(&completed);
        let worker_cancellation = cancellation.clone();
        let termination = runtime.termination_handle();
        let watcher = std::thread::spawn(move || {
            if worker_cancellation.wait_for_cancellation_or_completion(&worker_completed) {
                termination.terminate();
            }
        });
        Self {
            completed,
            cancellation: cancellation.clone(),
            watcher: Some(watcher),
        }
    }
}

impl Drop for RuntimeCancellationGuard {
    fn drop(&mut self) {
        self.completed.store(true, Ordering::Release);
        self.cancellation.notify_waiters();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

struct BrowserExecutionCancellationGuard {
    completed: Arc<AtomicBool>,
    cancellation: TestCancellationToken,
    watcher: Option<std::thread::JoinHandle<()>>,
}

impl BrowserExecutionCancellationGuard {
    fn arm(page: Arc<BrowserPage>, cancellation: &TestCancellationToken) -> Self {
        let completed = Arc::new(AtomicBool::new(false));
        let worker_completed = Arc::clone(&completed);
        let worker_cancellation = cancellation.clone();
        let watcher = std::thread::spawn(move || {
            if worker_cancellation.wait_for_cancellation_or_completion(&worker_completed) {
                let _ = page.command_with_timeout(
                    "Runtime.terminateExecution",
                    serde_json::json!({}),
                    std::time::Duration::from_millis(500),
                );
                let _ = page.command_with_timeout(
                    "Page.stopLoading",
                    serde_json::json!({}),
                    std::time::Duration::from_millis(500),
                );
            }
        });
        Self {
            completed,
            cancellation: cancellation.clone(),
            watcher: Some(watcher),
        }
    }
}

impl Drop for BrowserExecutionCancellationGuard {
    fn drop(&mut self) {
        self.completed.store(true, Ordering::Release);
        self.cancellation.notify_waiters();
        if let Some(watcher) = self.watcher.take() {
            let _ = watcher.join();
        }
    }
}

#[derive(Debug)]
pub struct TestError {
    code: &'static str,
    message: String,
    path: Option<PathBuf>,
}

impl TestError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
        }
    }

    fn at(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl fmt::Display for TestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(path) = &self.path {
            write!(formatter, "{}: {}", path.display(), self.message)
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for TestError {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSuiteResult {
    schema_version: String,
    status: TestStatus,
    cases: Vec<RuntimeCaseResult>,
    failures: Vec<RuntimeFailure>,
    snapshots: Vec<RuntimeSnapshot>,
    #[serde(default)]
    leaks: Vec<RuntimeLeak>,
    #[serde(default)]
    diagnostics: Vec<RuntimeDiagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeCaseResult {
    name: String,
    full_name: String,
    status: TestStatus,
    duration_ms: u64,
    failures: Vec<RuntimeFailure>,
    assertions: usize,
    #[serde(default)]
    registration_stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeFailure {
    message: String,
    code: Option<String>,
    stack: Option<String>,
    diff: Option<TestDiff>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeLeak {
    kind: String,
    description: String,
    stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeDiagnostic {
    code: String,
    message: String,
    stack: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshot {
    key: String,
    value: String,
}

const RUNTIME_RESULT_SCHEMA: &str = "wake.test.runtime.v1";
const RUNTIME_SCHEDULER_SCHEMA: &str = "wake.test.scheduler.v1";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSchedulerPlan {
    schema_version: String,
    cases: Vec<RuntimeSchedulerCase>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSchedulerCase {
    id: String,
    index: usize,
    name: String,
    full_name: String,
    status: String,
    registration_stack: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSchedulerCursor {
    schema_version: String,
    status: String,
    #[serde(default)]
    step: Option<RuntimeSchedulerStep>,
    #[serde(default)]
    partial_result: Option<RuntimeSuiteResult>,
    #[serde(default)]
    result: Option<RuntimeSuiteResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSchedulerStep {
    id: String,
    kind: String,
    suite_id: String,
    case_index: Option<usize>,
    case_name: Option<String>,
    case_full_name: Option<String>,
    timeout_ms: u64,
    registration_stack: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeSchedulerStepAck {
    schema_version: String,
    step_id: String,
    timed_out: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OriginalCoverageRange {
    start_byte: u32,
    end_byte: u32,
}

#[derive(Debug, Clone)]
struct NormalizedCoverageFile {
    path: String,
    source: String,
    lines: BTreeMap<u32, bool>,
    functions: BTreeMap<OriginalCoverageRange, bool>,
    blocks: BTreeMap<OriginalCoverageRange, bool>,
    /// Effective V8 coverage at each original source-map anchor for this one execution.
    ///
    /// V8 omits a nested range when its count equals its parent. The aggregate layer uses this map
    /// only when an identity is absent from this execution's explicit function/block ranges.
    range_anchor_hits: BTreeMap<u32, bool>,
}

#[derive(Debug, Clone)]
struct CoverageReportFile {
    path: String,
    source: String,
    lines: BTreeMap<u32, bool>,
    functions: BTreeMap<OriginalCoverageRange, bool>,
    blocks: BTreeMap<OriginalCoverageRange, bool>,
}

struct AggregatedCoverage {
    result: CoverageResult,
    report_files: Vec<CoverageReportFile>,
}

struct ExecutedSuite {
    suite: TestSuiteResult,
    coverage: Vec<NormalizedCoverageFile>,
    leaks: Vec<TestLeak>,
    diagnostics: Vec<TestDiagnostic>,
    artifacts: Vec<TestArtifact>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReactVersions {
    react: String,
    react_dom: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotUpdate {
    None,
    New,
    All,
}

/// Persistent, host-owned workspace state for one Wake Test context.
///
/// Every execution still creates a fresh JavaScript realm/browser context. Only deterministic
/// discovery records and compiled module graphs cross run boundaries.
#[doc(hidden)]
pub struct TestWorkspaceSession {
    options: TestOptions,
    seed: String,
    cache: TestRunCache,
}

#[derive(Default)]
struct TestRunCache {
    root: Option<PathBuf>,
    discovery_key: Option<String>,
    discovered: Option<Vec<DiscoveredTest>>,
    prepared: BTreeMap<SuiteIdentity, PreparedTest>,
    watch_paths: BTreeMap<PathBuf, WatchPathMetadata>,
}

#[derive(Debug, Clone, Copy)]
struct WatchPathMetadata {
    recursive: bool,
    roles: TestWatchPathRoles,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestWatchPath {
    pub path: PathBuf,
    pub recursive: bool,
    pub roles: TestWatchPathRoles,
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TestWatchPathRoles(u8);

impl TestWatchPathRoles {
    pub const PROJECT_TREE: Self = Self(1 << 0);
    pub const COMPILER_INPUT: Self = Self(1 << 1);
    pub const BASELINE_INPUT: Self = Self(1 << 2);

    #[must_use]
    pub const fn contains(self, roles: Self) -> bool {
        self.0 & roles.0 == roles.0
    }

    #[must_use]
    pub const fn union(self, roles: Self) -> Self {
        Self(self.0 | roles.0)
    }
}

/// The watcher keeps invalidation, topology rediscovery, and suite selection as three independent
/// decisions. This is host-internal and deliberately absent from the JavaScript API.
#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub struct TestWatchRequest {
    pub invalidated_paths: Vec<PathBuf>,
    pub selection: TestWatchSelection,
    pub rediscover: bool,
}

#[doc(hidden)]
#[derive(Debug, Clone, Default)]
pub enum TestWatchSelection {
    /// Apply the context's configured `changed`/`related` selection, if any.
    Configured,
    /// Select suites affected by `TestWatchRequest::invalidated_paths`.
    Affected,
    /// Execute every discovered suite.
    #[default]
    All,
    /// Select suites related to these explicit physical paths.
    Related(Vec<PathBuf>),
    /// Select exact project-qualified suites (used by the native failed-suite watch mode).
    Suites(Vec<TestWatchSuite>),
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestWatchSuite {
    pub path: PathBuf,
    pub project: Option<String>,
}

impl TestWorkspaceSession {
    pub fn new(options: TestOptions) -> Self {
        let seed = options
            .seed
            .clone()
            .unwrap_or_else(|| random_seed().to_string());
        Self {
            options,
            seed,
            cache: TestRunCache::default(),
        }
    }

    fn normalize_options(&mut self, mut options: TestOptions) -> TestOptions {
        if let Some(seed) = options.seed.as_ref() {
            self.seed.clone_from(seed);
        } else {
            options.seed = Some(self.seed.clone());
        }
        options
    }

    pub fn run(
        &mut self,
        options: TestOptions,
        cancellation: TestCancellationToken,
    ) -> Result<TestRunResult, TestError> {
        self.options = self.normalize_options(options);
        // A manually requested run has no watcher-owned invalidation record. Rediscover topology
        // and recompile so creating or deleting a suite between calls is observable.
        self.cache.discovered = None;
        self.cache.prepared.clear();
        run_tests_with_selection_cached(self.options.clone(), cancellation, None, &mut self.cache)
    }

    pub fn run_watch(
        &mut self,
        options: TestOptions,
        request: TestWatchRequest,
        cancellation: TestCancellationToken,
    ) -> Result<TestRunResult, TestError> {
        self.options = self.normalize_options(options);
        run_tests_with_selection_cached(
            self.options.clone(),
            cancellation,
            Some(request),
            &mut self.cache,
        )
    }

    /// Physical compiler/discovery inputs that the host must keep subscribed while watching.
    #[doc(hidden)]
    pub fn watch_paths(&self) -> Vec<TestWatchPath> {
        self.cache
            .watch_paths
            .iter()
            .map(|(path, metadata)| TestWatchPath {
                path: path.clone(),
                recursive: metadata.recursive,
                roles: metadata.roles,
            })
            .collect()
    }
}

/// Discover and execute one Wake-native test run.
pub fn run_tests(options: TestOptions) -> Result<TestRunResult, TestError> {
    run_tests_with_cancellation(options, TestCancellationToken::default())
}

/// Discover and execute one Wake-native test run with an interruptible VM/browser seam.
#[doc(hidden)]
pub fn run_tests_with_cancellation(
    options: TestOptions,
    cancellation: TestCancellationToken,
) -> Result<TestRunResult, TestError> {
    run_tests_with_selection(options, cancellation, None)
}

/// Execute one watcher-owned invalidation round. Unknown or structural paths select all suites so
/// an incomplete graph can never cause a false-negative.
#[doc(hidden)]
pub fn run_tests_for_watch(
    options: TestOptions,
    changed_paths: Vec<PathBuf>,
    cancellation: TestCancellationToken,
) -> Result<TestRunResult, TestError> {
    let request = TestWatchRequest {
        invalidated_paths: changed_paths,
        selection: TestWatchSelection::Affected,
        rediscover: false,
    };
    run_tests_with_selection(options, cancellation, Some(request))
}

fn run_tests_with_selection(
    options: TestOptions,
    cancellation: TestCancellationToken,
    watch_request: Option<TestWatchRequest>,
) -> Result<TestRunResult, TestError> {
    run_tests_with_selection_cached(
        options,
        cancellation,
        watch_request,
        &mut TestRunCache::default(),
    )
}

fn run_tests_with_selection_cached(
    options: TestOptions,
    cancellation: TestCancellationToken,
    watch_request: Option<TestWatchRequest>,
    cache: &mut TestRunCache,
) -> Result<TestRunResult, TestError> {
    let started = Instant::now();
    let root = match options.root.clone() {
        Some(path) => path,
        None => std::env::current_dir().map_err(|error| {
            TestError::new("WAKE_TEST_CONFIG", format!("could not read cwd: {error}"))
        })?,
    };
    let root = root.canonicalize().unwrap_or(root);
    let config = wake_config::load(&root)
        .map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()).at(&root))?;
    let coverage_enabled = options.coverage || config.test.coverage.enabled;
    if let Some(environment) = options.environment.as_deref()
        && !matches!(environment, "auto" | "dom" | "browser")
    {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            format!("unknown test environment {environment:?}"),
        ));
    }
    let seed = options
        .seed
        .clone()
        .unwrap_or_else(|| random_seed().to_string());
    let test_name_pattern = compile_optional_pattern(options.name_pattern.as_deref())?;
    if options.changed && !options.related.is_empty() {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            "changed and related test selection cannot be combined",
        ));
    }
    let config_source = fs::read_to_string(root.join("wake.config.toml")).unwrap_or_default();
    let discovery_key = serde_json::to_string(&serde_json::json!({
        "patterns": &options.patterns,
        "projects": &options.projects,
        "environment": options.environment.as_deref(),
        "config": config_source,
    }))
    .expect("test discovery key is serializable");
    if cache.root.as_ref() != Some(&root)
        || cache.discovery_key.as_deref() != Some(discovery_key.as_str())
    {
        cache.root = Some(root.clone());
        cache.discovery_key = Some(discovery_key);
        cache.discovered = None;
        cache.prepared.clear();
        cache.watch_paths.clear();
    }
    if watch_request
        .as_ref()
        .is_some_and(|request| request.rediscover)
    {
        cache.discovered = None;
        cache.prepared.clear();
    }
    if let Some(paths) = watch_request
        .as_ref()
        .map(|request| request.invalidated_paths.as_slice())
        .filter(|paths| !paths.is_empty())
    {
        if cache.prepared.is_empty() {
            // This includes a previously empty discovery result. Any filesystem notification may
            // be the first suite, so a warm empty cache is never authoritative.
            cache.discovered = None;
            cache.prepared.clear();
        } else {
            let mut previous_index = SuiteGraphIndex::default();
            for prepared in cache.prepared.values() {
                match &prepared.graph {
                    Ok(graph) => previous_index.record(
                        &root,
                        &prepared.discovered.path,
                        prepared.discovered.project.as_deref(),
                        graph.module_graph(),
                    ),
                    Err(_) => previous_index.record_opaque(
                        &root,
                        &prepared.discovered.path,
                        prepared.discovered.project.as_deref(),
                    ),
                }
            }
            let invalidation = previous_index.select(&root, paths, RelatedOrigin::Watch);
            if invalidation.conservative || paths.iter().any(|path| !path.exists()) {
                cache.discovered = None;
                cache.prepared.clear();
            } else {
                for suite in invalidation.suites {
                    cache.prepared.remove(&suite);
                }
            }
        }
    }
    let discovered = match &cache.discovered {
        Some(discovered) => discovered.clone(),
        None => {
            let discovered = discover_tests(
                &root,
                &config.test,
                &options.patterns,
                &options.projects,
                options.environment.as_deref(),
            )?;
            cache.discovered = Some(discovered.clone());
            discovered
        }
    };
    cache.watch_paths = configured_test_watch_roots(&root, &config.test, &options.projects)?
        .into_iter()
        .map(|path| {
            (
                path,
                WatchPathMetadata {
                    recursive: true,
                    roles: TestWatchPathRoles::PROJECT_TREE,
                },
            )
        })
        .collect();
    cache
        .watch_paths
        .entry(root.join("wake.config.toml"))
        .or_insert(WatchPathMetadata {
            recursive: false,
            roles: TestWatchPathRoles::COMPILER_INPUT,
        });
    let mut graph_index = SuiteGraphIndex::default();
    let mut test_paths = discovered
        .into_iter()
        .map(|suite| {
            let identity = suite.identity(&root);
            cache.prepared.get(&identity).cloned().unwrap_or_else(|| {
                let prepared = prepare_test(&root, suite);
                cache.prepared.insert(identity, prepared.clone());
                prepared
            })
        })
        .inspect(|prepared| match &prepared.graph {
            Ok(graph) => graph_index.record(
                &root,
                &prepared.discovered.path,
                prepared.discovered.project.as_deref(),
                graph.module_graph(),
            ),
            Err(_) => graph_index.record_opaque(
                &root,
                &prepared.discovered.path,
                prepared.discovered.project.as_deref(),
            ),
        })
        .collect::<Vec<_>>();
    let discovered_identities = test_paths
        .iter()
        .map(|prepared| prepared.discovered.identity(&root))
        .collect::<BTreeSet<_>>();
    cache
        .prepared
        .retain(|identity, _| discovered_identities.contains(identity));
    refresh_compiled_watch_paths(&root, &test_paths, &mut cache.watch_paths);
    let watch_selection = watch_request.as_ref().map(|request| &request.selection);
    let selection_active = match watch_selection {
        Some(TestWatchSelection::All) => false,
        Some(
            TestWatchSelection::Affected
            | TestWatchSelection::Related(_)
            | TestWatchSelection::Suites(_),
        ) => true,
        Some(TestWatchSelection::Configured) | None => {
            options.changed || !options.related.is_empty()
        }
    };
    let mut selection_diagnostics = Vec::new();
    if selection_active {
        if let Some(TestWatchSelection::Suites(suites)) = watch_selection {
            let identities = suites
                .iter()
                .map(|suite| SuiteIdentity::new(&root, &suite.path, suite.project.as_deref()))
                .collect::<BTreeSet<_>>();
            test_paths.retain(|suite| identities.contains(&suite.discovered.identity(&root)));
        } else {
            let (related, origin) = match watch_selection {
                Some(TestWatchSelection::Affected) => (
                    watch_request
                        .as_ref()
                        .map(|request| request.invalidated_paths.clone())
                        .unwrap_or_default(),
                    RelatedOrigin::Watch,
                ),
                Some(TestWatchSelection::Related(paths)) => {
                    (paths.clone(), RelatedOrigin::Explicit)
                }
                Some(TestWatchSelection::Configured) | None if options.changed => {
                    let paths = git_changed::changed_paths(&root).map_err(|error| {
                        TestError::new(
                            "WAKE_TEST_DISCOVERY",
                            format!(
                                "changed-file discovery failed ({:?}): {}",
                                error.kind(),
                                error.message()
                            ),
                        )
                        .at(&root)
                    })?;
                    (paths, RelatedOrigin::Changed)
                }
                Some(TestWatchSelection::Configured) | None => {
                    (options.related.clone(), RelatedOrigin::Explicit)
                }
                Some(TestWatchSelection::All) => unreachable!("all selection is not active"),
                Some(TestWatchSelection::Suites(_)) => {
                    unreachable!("exact suite selection is handled before graph selection")
                }
            };
            let selection = graph_index.select(&root, &related, origin);
            test_paths.retain(|suite| selection.suites.contains(&suite.discovered.identity(&root)));
            for reason in selection.reasons {
                selection_diagnostics.push(TestDiagnostic {
                    severity: DiagnosticSeverity::Note,
                    code: "WAKE_TEST_DISCOVERY".to_string(),
                    message: format!("Conservative dependency selection: {reason}"),
                    path: Some(normalize_path(&root)),
                    location: None,
                    notes: Vec::new(),
                });
            }
            if test_paths.is_empty() {
                selection_diagnostics.push(TestDiagnostic {
                    severity: DiagnosticSeverity::Note,
                    code: "WAKE_TEST_DISCOVERY".to_string(),
                    message: "No tests are related to the selected paths".to_string(),
                    path: Some(normalize_path(&root)),
                    location: None,
                    notes: Vec::new(),
                });
            }
        }
    }
    if options.shuffle {
        test_paths.sort_by_key(|suite| seeded_path_key(&suite.discovered.path, &seed));
    }
    if let Some(shard) = options.shard.as_deref() {
        let (index, count) = parse_shard(shard)?;
        test_paths = test_paths
            .into_iter()
            .enumerate()
            .filter_map(|(position, suite)| (position % count == index - 1).then_some(suite))
            .collect();
    }
    let discovered_count = test_paths.len();
    let browser = (!cancellation.is_cancelled()
        && test_paths
            .iter()
            .any(|suite| suite.discovered.environment == wake_config::TestEnvironment::Browser))
    .then(|| launch_browser(&config.test, &options))
    .transpose()?
    .map(Arc::new);

    let primary_kind = if test_paths
        .iter()
        .all(|suite| suite.discovered.environment == wake_config::TestEnvironment::Browser)
    {
        "browser"
    } else {
        "dom"
    };
    let mut result = TestRunResult::empty(
        format!("wake-test-{}-{}", std::process::id(), random_seed()),
        seed.clone(),
        environment_info(primary_kind, None, browser.as_deref()),
    );
    result.diagnostics.append(&mut selection_diagnostics);
    let bail = options.bail.unwrap_or(0);
    let suites = if !options.serial && bail == 0 && test_paths.len() > 1 {
        execute_suites_parallel(
            &root,
            &test_paths,
            &options,
            test_name_pattern.as_ref(),
            &seed,
            browser.as_ref(),
            coverage_enabled,
            &cancellation,
            worker_count(
                options.workers.as_ref(),
                &config.test.workers,
                test_paths.len(),
            )?,
        )?
    } else {
        let mut suites = Vec::new();
        for prepared in test_paths {
            if cancellation.is_cancelled() {
                break;
            }
            let suite = execute_suite(
                &root,
                &prepared,
                &options,
                test_name_pattern.as_ref(),
                &seed,
                browser.as_deref(),
                coverage_enabled,
                &cancellation,
            )?;
            let failed = suite.suite.status == TestStatus::Failed;
            suites.push(suite);
            if failed
                && bail > 0
                && suites
                    .iter()
                    .filter(|suite| suite.suite.status == TestStatus::Failed)
                    .count()
                    >= bail as usize
            {
                break;
            }
        }
        suites
    };
    let mut coverage_files = Vec::new();
    for executed in suites {
        let suite = executed.suite;
        coverage_files.extend(executed.coverage);
        result.leaks.extend(executed.leaks);
        result.diagnostics.extend(executed.diagnostics);
        result.artifacts.extend(executed.artifacts);
        if let Some(snapshot) = &suite.snapshot {
            merge_snapshot_summary(&mut result.snapshot, snapshot);
        }
        result.suites.push(suite);
    }
    let mut coverage_failed = false;
    if coverage_enabled && !cancellation.is_cancelled() {
        let aggregated = aggregate_coverage(coverage_files);
        let artifacts = write_coverage_reports(
            &root,
            &config.test.coverage.reporters,
            &aggregated.result,
            &aggregated.report_files,
        )?;
        let artifact_ids = artifacts
            .iter()
            .map(|artifact| artifact.id.clone())
            .collect();
        let mut coverage = aggregated.result;
        coverage.report_artifact_ids = artifact_ids;
        let threshold_diagnostics =
            coverage_threshold::evaluate(&root, &config.test.coverage, &coverage)?;
        coverage_failed = !threshold_diagnostics.is_empty();
        result.diagnostics.extend(threshold_diagnostics);
        result.artifacts.extend(artifacts);
        result.coverage = Some(coverage);
    }
    let mut react_versions = result.suites.iter().filter_map(|suite| {
        suite.environment.as_ref().and_then(|environment| {
            Some((environment.react.clone()?, environment.react_dom.clone()?))
        })
    });
    if let Some((react, react_dom)) = react_versions.next()
        && react_versions.all(|candidate| candidate == (react.clone(), react_dom.clone()))
    {
        result.environment.react = Some(react);
        result.environment.react_dom = Some(react_dom);
    }

    result.counts.suites.total = result.suites.len();
    for suite in &result.suites {
        match suite.status {
            TestStatus::Failed => result.counts.suites.failed += 1,
            TestStatus::Skipped => result.counts.suites.skipped += 1,
            TestStatus::Passed => result.counts.suites.passed += 1,
            TestStatus::Todo => result.counts.suites.skipped += 1,
        }
        for test in &suite.tests {
            result.counts.tests.total += 1;
            match test.status {
                TestStatus::Passed => result.counts.tests.passed += 1,
                TestStatus::Failed => result.counts.tests.failed += 1,
                TestStatus::Skipped => result.counts.tests.skipped += 1,
                TestStatus::Todo => result.counts.tests.todo += 1,
            }
        }
    }
    result.success = result.counts.suites.failed == 0
        && !coverage_failed
        && (!result.suites.is_empty() || options.allow_no_tests || selection_active);
    if bail > 0
        && result.counts.suites.failed >= bail as usize
        && result.suites.len() < discovered_count
    {
        result.termination_reason = TestTerminationReason::Bail;
    }
    if result.suites.is_empty()
        && !options.allow_no_tests
        && !selection_active
        && !cancellation.is_cancelled()
    {
        result.diagnostics.push(TestDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "WAKE_TEST_DISCOVERY".to_string(),
            message: "No tests found".to_string(),
            path: Some(normalize_path(&root)),
            location: None,
            notes: Vec::new(),
        });
    }
    if cancellation.is_cancelled() {
        result.success = false;
        result.termination_reason = TestTerminationReason::Cancelled;
    }
    result.duration_ms = duration_ms(started);
    Ok(result)
}

fn worker_count(
    override_value: Option<&WorkerOverride>,
    configured: &wake_config::WorkerCount,
    suites: usize,
) -> Result<usize, TestError> {
    let available = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let requested = match override_value {
        Some(WorkerOverride::Count(0)) => {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                "workers must be greater than zero",
            ));
        }
        Some(WorkerOverride::Count(count)) => *count,
        Some(WorkerOverride::Text(value)) if value == "auto" => available,
        Some(WorkerOverride::Text(value)) => value
            .strip_suffix('%')
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| (1..=100).contains(value))
            .map(|value| available.saturating_mul(value).div_ceil(100))
            .ok_or_else(|| {
                TestError::new(
                    "WAKE_TEST_CONFIG",
                    "workers must be a positive integer, auto, or 1%-100%",
                )
            })?,
        None => match configured {
            wake_config::WorkerCount::Auto => available,
            wake_config::WorkerCount::Count(count) => *count,
            wake_config::WorkerCount::Percent(percent) => available
                .saturating_mul(usize::from(*percent))
                .div_ceil(100),
        },
    };
    Ok(requested.max(1).min(suites.max(1)))
}

fn execute_suites_parallel(
    root: &Path,
    discovered: &[PreparedTest],
    options: &TestOptions,
    test_name_pattern: Option<&Regex>,
    seed: &str,
    browser: Option<&Arc<BrowserDriver>>,
    coverage_enabled: bool,
    cancellation: &TestCancellationToken,
    workers: usize,
) -> Result<Vec<ExecutedSuite>, TestError> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(discovered.len())
            .collect::<Vec<Option<Result<ExecutedSuite, TestError>>>>(),
    );
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    if cancellation.is_cancelled() {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(suite) = discovered.get(index) else {
                        break;
                    };
                    let value = execute_suite(
                        root,
                        suite,
                        options,
                        test_name_pattern,
                        seed,
                        browser.map(Arc::as_ref),
                        coverage_enabled,
                        cancellation,
                    );
                    results.lock().expect("test result lock is not poisoned")[index] = Some(value);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("test result lock is not poisoned")
        .into_iter()
        .flatten()
        .collect()
}

fn execute_suite(
    root: &Path,
    prepared: &PreparedTest,
    options: &TestOptions,
    test_name_pattern: Option<&Regex>,
    seed: &str,
    browser: Option<&BrowserDriver>,
    coverage_enabled: bool,
    cancellation: &TestCancellationToken,
) -> Result<ExecutedSuite, TestError> {
    let path = &prepared.discovered.path;
    let config = &prepared.discovered.config;
    let project = prepared.discovered.project.as_deref();
    let path_label = path
        .strip_prefix(root)
        .map_or_else(|_| normalize_path(path), normalize_path);
    let suite_identity_label = suite_identity_label(&path_label, project);
    let suite_started = Instant::now();
    let react_versions = prepared
        .original_source
        .contains("@crab-dev/wake/test/react")
        .then(|| validate_react_versions(path))
        .transpose()?;
    let graph = match prepared.graph() {
        Ok(graph) => graph,
        Err(error) => {
            return Ok(failed_prepared_suite(
                prepared,
                path_label,
                suite_identity_label,
                browser,
                suite_started,
                error,
            ));
        }
    };
    let snapshot_path = snapshot_path(path, &config.snapshot.directory, project);
    let expected_snapshots = load_snapshots(&snapshot_path)?;
    let update_mode = match options.update_snapshots.as_deref().unwrap_or("new") {
        "none" => SnapshotUpdate::None,
        "new" => SnapshotUpdate::New,
        "all" => SnapshotUpdate::All,
        value => {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                format!("unknown snapshot update mode {value:?}"),
            ));
        }
    };
    let configuration = serde_json::json!({
        "seed": seed,
        "timeoutMs": config.timeout_ms,
        "forbidOnly": config.forbid_only,
        "reactStrictMode": config.react.strict_mode,
        "reactCleanup": config.react.cleanup,
        "reactActWarnings": match config.react.act_warnings {
            wake_config::TestDiagnosticPolicy::Off => "off",
            wake_config::TestDiagnosticPolicy::Warn => "warn",
            wake_config::TestDiagnosticPolicy::Error => "error",
        },
        "testIdAttribute": config.react.test_id_attribute,
        "environment": match config.environment {
            wake_config::TestEnvironment::Browser => "browser",
            wake_config::TestEnvironment::Auto | wake_config::TestEnvironment::Dom => "dom",
        },
        "networkAllowHosts": config.network.allow_hosts,
        "networkMode": match config.network.mode {
            wake_config::TestNetworkMode::Deny => "deny",
            wake_config::TestNetworkMode::Allow => "allow",
        },
        "namePattern": test_name_pattern.map(Regex::as_str),
        "snapshots": expected_snapshots,
        "updateSnapshots": match update_mode {
            SnapshotUpdate::All => "all",
            SnapshotUpdate::New => "new",
            SnapshotUpdate::None => "none",
        },
    });
    let host_prelude = if config.environment == wake_config::TestEnvironment::Browser {
        BROWSER_HOST_PRELUDE
    } else {
        ""
    };
    let prelude = format!(
        "{host_prelude}\n{WAKE_TEST_RUNTIME}\nglobalThis.__wakeConfigureTest({configuration});"
    );
    let completion =
        "globalThis.__wakePrepareTestRunAfterModules(globalThis.__wakeEntryModulePromise);";
    let compiled = emit_commonjs_graph_script(graph, &prepared.entry_path, &prelude, completion);
    let (runtime_result, raw_coverage, mut browser_operations) =
        if config.environment == wake_config::TestEnvironment::Browser {
            let browser = browser.ok_or_else(|| {
                TestError::new(
                    "WAKE_TEST_BROWSER",
                    "browser suite has no active browser driver",
                )
            })?;
            execute_browser_graph(
                browser,
                &compiled,
                coverage_enabled,
                config.timeout_ms,
                path,
                BrowserScreenshotPolicy::new(
                    path,
                    &suite_identity_label,
                    &config.snapshot.screenshot_directory,
                    update_mode,
                    browser,
                ),
                cancellation,
            )?
        } else {
            let (result, coverage) =
                execute_dom_graph(&compiled, coverage_enabled, config.timeout_ms, cancellation)?;
            (result, coverage, BrowserOperationOutput::default())
        };
    let source_mapper = GraphSourceMapper::new(root, path, &compiled);
    let internal: RuntimeSuiteResult = match runtime_result {
        Ok(json) => serde_json::from_str(&json).map_err(|error| {
            TestError::new(
                "WAKE_TEST_HOST",
                format!("test runtime returned invalid JSON: {error}"),
            )
            .at(path)
        })?,
        Err(error) => {
            let message = if error.to_string().contains("terminated")
                || error.to_string().contains("timeout")
            {
                format!(
                    "WAKE_TEST_TIMEOUT: test execution exceeded {} ms",
                    config.timeout_ms
                )
            } else {
                error.to_string()
            };
            let mut runtime_failure = failure(message);
            runtime_failure.location = runtime_failure
                .stack
                .as_deref()
                .and_then(|stack| source_mapper.map_stack(stack));
            let mut failures = vec![runtime_failure];
            failures.extend(browser_operations.failures.drain(..).map(failure));
            return Ok(ExecutedSuite {
                suite: TestSuiteResult {
                    id: stable_id("suite", &suite_identity_label),
                    path: path_label,
                    name: path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .map(str::to_string),
                    project: project.map(str::to_string),
                    environment: Some(environment_info(
                        match config.environment {
                            wake_config::TestEnvironment::Browser => "browser",
                            wake_config::TestEnvironment::Auto
                            | wake_config::TestEnvironment::Dom => "dom",
                        },
                        react_versions.as_ref(),
                        browser,
                    )),
                    status: TestStatus::Failed,
                    duration_ms: duration_ms(suite_started),
                    tests: Vec::new(),
                    failures,
                    snapshot: Some(browser_operations.snapshot),
                },
                coverage: normalize_coverage(root, path, &compiled, raw_coverage.as_ref()),
                leaks: Vec::new(),
                diagnostics: Vec::new(),
                artifacts: browser_operations.artifacts,
            });
        }
    };
    if internal.schema_version != RUNTIME_RESULT_SCHEMA {
        return Err(TestError::new(
            "WAKE_TEST_HOST",
            format!(
                "test runtime returned unsupported result schema {:?}",
                internal.schema_version
            ),
        )
        .at(path));
    }

    let (mut snapshot, snapshot_failures) = reconcile_snapshots(
        &snapshot_path,
        expected_snapshots,
        &internal.snapshots,
        update_mode,
    )?;
    let mut failures = internal
        .failures
        .into_iter()
        .map(|failure| structured_failure(failure, &source_mapper))
        .collect::<Vec<_>>();
    failures.extend(snapshot_failures.into_iter().map(failure));
    failures.extend(browser_operations.failures.drain(..).map(failure));
    merge_snapshot_summary(&mut snapshot, &browser_operations.snapshot);
    let mut leaks = internal
        .leaks
        .into_iter()
        .map(|leak| TestLeak {
            location: leak
                .stack
                .as_deref()
                .and_then(|stack| source_mapper.map_stack(stack)),
            kind: leak.kind,
            description: leak.description,
            stack: leak.stack,
        })
        .collect::<Vec<_>>();
    let leak_severity = match config.leaks {
        wake_config::TestLeakPolicy::Off => {
            leaks.clear();
            None
        }
        wake_config::TestLeakPolicy::Warn => Some(DiagnosticSeverity::Warning),
        wake_config::TestLeakPolicy::Error => Some(DiagnosticSeverity::Error),
    };
    let mut diagnostics = leak_severity.map_or_else(Vec::new, |severity| {
        leaks
            .iter()
            .map(|leak| TestDiagnostic {
                severity,
                code: "WAKE_TEST_LEAK".to_string(),
                message: leak.description.clone(),
                path: Some(path_label.clone()),
                location: leak.location.clone(),
                notes: Vec::new(),
            })
            .collect::<Vec<_>>()
    });
    let act_warning_severity = match config.react.act_warnings {
        wake_config::TestDiagnosticPolicy::Off => None,
        wake_config::TestDiagnosticPolicy::Warn => Some(DiagnosticSeverity::Warning),
        wake_config::TestDiagnosticPolicy::Error => Some(DiagnosticSeverity::Error),
    };
    if let Some(severity) = act_warning_severity {
        diagnostics.extend(internal.diagnostics.into_iter().map(|diagnostic| {
            let location = diagnostic
                .stack
                .as_deref()
                .and_then(|stack| source_mapper.map_stack(stack));
            TestDiagnostic {
                severity,
                code: diagnostic.code,
                message: diagnostic.message,
                path: Some(path_label.clone()),
                location,
                notes: Vec::new(),
            }
        }));
    }
    if config.leaks == wake_config::TestLeakPolicy::Error {
        failures.extend(leaks.iter().map(|leak| TestFailure {
            message: leak.description.clone(),
            code: Some("WAKE_TEST_LEAK".to_string()),
            stack: leak.stack.clone(),
            location: leak.location.clone(),
            diff: None,
        }));
    }
    let tests = internal
        .cases
        .into_iter()
        .map(|test| {
            let location = test
                .registration_stack
                .as_deref()
                .and_then(|stack| source_mapper.map_stack(stack));
            let failures = test
                .failures
                .into_iter()
                .map(|failure| structured_failure(failure, &source_mapper))
                .collect::<Vec<_>>();
            let location =
                location.or_else(|| failures.iter().find_map(|failure| failure.location.clone()));
            TestCaseResult {
                id: stable_id(
                    "test",
                    &format!("{}:{}", suite_identity_label, test.full_name),
                ),
                name: test.name,
                full_name: test.full_name,
                status: test.status,
                duration_ms: test.duration_ms,
                assertions: test.assertions,
                attempts: 1,
                location,
                failures,
            }
        })
        .collect::<Vec<_>>();
    let failed = internal.status == TestStatus::Failed
        || tests.iter().any(|test| test.status == TestStatus::Failed)
        || !failures.is_empty();
    Ok(ExecutedSuite {
        suite: TestSuiteResult {
            id: stable_id("suite", &suite_identity_label),
            path: path_label,
            name: path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_string),
            project: project.map(str::to_string),
            environment: Some(environment_info(
                match config.environment {
                    wake_config::TestEnvironment::Browser => "browser",
                    wake_config::TestEnvironment::Auto | wake_config::TestEnvironment::Dom => "dom",
                },
                react_versions.as_ref(),
                browser,
            )),
            status: if failed {
                TestStatus::Failed
            } else {
                TestStatus::Passed
            },
            duration_ms: duration_ms(suite_started),
            tests,
            failures,
            snapshot: Some(snapshot),
        },
        coverage: normalize_coverage(root, path, &compiled, raw_coverage.as_ref()),
        leaks,
        diagnostics,
        artifacts: browser_operations.artifacts,
    })
}

fn failed_prepared_suite(
    prepared: &PreparedTest,
    path_label: String,
    suite_identity_label: String,
    browser: Option<&BrowserDriver>,
    suite_started: Instant,
    error: TestError,
) -> ExecutedSuite {
    let path = &prepared.discovered.path;
    let config = &prepared.discovered.config;
    ExecutedSuite {
        suite: TestSuiteResult {
            id: stable_id("suite", &suite_identity_label),
            path: path_label,
            name: path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_string),
            project: prepared.discovered.project.clone(),
            environment: Some(environment_info(
                match config.environment {
                    wake_config::TestEnvironment::Browser => "browser",
                    wake_config::TestEnvironment::Auto | wake_config::TestEnvironment::Dom => "dom",
                },
                None,
                browser,
            )),
            status: TestStatus::Failed,
            duration_ms: duration_ms(suite_started),
            tests: Vec::new(),
            failures: vec![TestFailure {
                message: error.to_string(),
                code: Some(error.code().to_string()),
                stack: None,
                location: None,
                diff: None,
            }],
            snapshot: Some(SnapshotSummary::default()),
        },
        coverage: Vec::new(),
        leaks: Vec::new(),
        diagnostics: Vec::new(),
        artifacts: Vec::new(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GeneratedScriptLine {
    utf16_start: usize,
    utf16_end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StackFramePosition {
    source: String,
    line: usize,
    column: usize,
}

struct GraphSourceMapper<'a> {
    root: &'a Path,
    suite_path: &'a Path,
    compiled: &'a CompiledCommonJsGraphScript,
    script_identity: NormalizedScriptSource,
    generated_lines: Vec<GeneratedScriptLine>,
}

impl<'a> GraphSourceMapper<'a> {
    fn new(
        root: &'a Path,
        suite_path: &'a Path,
        compiled: &'a CompiledCommonJsGraphScript,
    ) -> Self {
        Self {
            root,
            suite_path,
            compiled,
            script_identity: normalize_stack_source(compiled.source_url())
                .expect("Wake graph script paths are valid UTF-8 source identities"),
            generated_lines: generated_script_lines(compiled.script()),
        }
    }

    fn map_stack(&self, stack: &str) -> Option<TestLocation> {
        let mut best = None;
        for (frame_index, frame) in stack.lines().enumerate() {
            let Some(frame) = parse_stack_frame(frame) else {
                continue;
            };
            let Some(frame_source) = normalize_stack_source(&frame.source) else {
                continue;
            };
            if !same_stack_source(&frame_source, &self.script_identity) {
                continue;
            }
            let Some(absolute_offset) = self.generated_offset(frame.line, frame.column) else {
                continue;
            };
            let Some((priority, location)) = self.map_generated_offset(absolute_offset) else {
                continue;
            };
            let replace = best.as_ref().is_none_or(|(best_priority, best_index, _)| {
                (priority, frame_index) < (*best_priority, *best_index)
            });
            if replace {
                best = Some((priority, frame_index, location));
            }
        }
        best.map(|(_, _, location)| location)
    }

    fn generated_offset(&self, line: usize, column: usize) -> Option<usize> {
        let line = self.generated_lines.get(line.checked_sub(1)?)?;
        let column = column.checked_sub(1)?;
        let offset = line.utf16_start.checked_add(column)?;
        (offset < line.utf16_end).then_some(offset)
    }

    fn map_generated_offset(&self, absolute_offset: usize) -> Option<(u8, TestLocation)> {
        self.compiled
            .modules()
            .iter()
            .filter(|module| !module.is_synthetic())
            .find_map(|module| {
                let body = module.body();
                if absolute_offset < body.utf16_start || absolute_offset >= body.utf16_end {
                    return None;
                }
                let original_byte = module.original_byte_offset_for_body_utf16_offset(
                    absolute_offset - body.utf16_start,
                )?;
                let original_byte = usize::try_from(original_byte).ok()?;
                if original_byte > module.original_source().len()
                    || !module.original_source().is_char_boundary(original_byte)
                {
                    return None;
                }
                let source = SourceFile::new(
                    normalize_path(module.source_path()),
                    module.original_source(),
                );
                let (line, column) = source.location0_utf16(u32::try_from(original_byte).ok()?);
                let line = usize::try_from(line).ok()?.checked_add(1)?;
                let column = usize::try_from(column).ok()?.checked_add(1)?;
                let path = module
                    .source_path()
                    .strip_prefix(self.root)
                    .map_or_else(|_| normalize_path(module.source_path()), normalize_path);
                Some((
                    self.module_priority(module.source_path()),
                    TestLocation {
                        path,
                        line,
                        column,
                        end_line: None,
                        end_column: None,
                    },
                ))
            })
    }

    fn module_priority(&self, source_path: &Path) -> u8 {
        if same_path_identity(source_path, self.suite_path) {
            0
        } else if source_path
            .components()
            .any(|component| component.as_os_str() == "node_modules")
        {
            3
        } else if source_path.strip_prefix(self.root).is_ok() {
            1
        } else {
            2
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedScriptSource {
    value: String,
    windows: bool,
}

fn normalize_stack_source(source: &str) -> Option<NormalizedScriptSource> {
    let source = source.trim();
    if source.is_empty() {
        return None;
    }
    let is_file_url = source
        .get(.."file://".len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"));
    let mut value = if is_file_url {
        percent_decode_utf8(&source["file://".len()..])?
    } else {
        source.to_string()
    }
    .replace('\\', "/");

    if is_file_url {
        if value
            .get(.."localhost".len())
            .is_some_and(|host| host.eq_ignore_ascii_case("localhost"))
            && value
                .as_bytes()
                .get("localhost".len())
                .is_some_and(|separator| *separator == b'/')
        {
            value = value["localhost".len()..].to_string();
        } else if !value.starts_with('/') && !is_windows_drive_path(&value) {
            value = format!("//{value}");
        }
    }

    if value.strip_prefix('/').is_some_and(is_windows_drive_path) {
        value.remove(0);
    }
    if value
        .get(.."//?/UNC/".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("//?/UNC/"))
    {
        value = format!("//{}", &value["//?/UNC/".len()..]);
    } else if value.starts_with("//?/") {
        value = value["//?/".len()..].to_string();
    }
    let windows = is_windows_drive_path(&value) || value.starts_with("//");
    Some(NormalizedScriptSource { value, windows })
}

fn same_stack_source(left: &NormalizedScriptSource, right: &NormalizedScriptSource) -> bool {
    if left.windows && right.windows {
        left.value.eq_ignore_ascii_case(&right.value)
    } else {
        left.value == right.value
    }
}

fn same_path_identity(left: &Path, right: &Path) -> bool {
    let left = normalize_stack_source(&normalize_path(left));
    let right = normalize_stack_source(&normalize_path(right));
    matches!((left, right), (Some(left), Some(right)) if same_stack_source(&left, &right))
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/'
}

fn percent_decode_utf8(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn parse_stack_frame(frame: &str) -> Option<StackFramePosition> {
    let payload = frame.trim().strip_prefix("at ")?.trim();
    let (location, wrapped) = payload
        .strip_suffix(')')
        .map_or((payload, false), |location| (location, true));
    let (location, column) = location.rsplit_once(':')?;
    let column = parse_positive_decimal(column)?;
    let (source, line) = location.rsplit_once(':')?;
    let line = parse_positive_decimal(line)?;
    let source = if wrapped {
        source.split_once(" (")?.1
    } else {
        source.strip_prefix("async ").unwrap_or(source)
    }
    .trim();
    (!source.is_empty()).then(|| StackFramePosition {
        source: source.to_string(),
        line,
        column,
    })
}

fn parse_positive_decimal(value: &str) -> Option<usize> {
    (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse::<usize>().ok())
        .flatten()
        .filter(|value| *value > 0)
}

fn generated_script_lines(script: &str) -> Vec<GeneratedScriptLine> {
    let mut lines = Vec::new();
    let mut line_start = 0_usize;
    let mut utf16_offset = 0_usize;
    let mut previous_was_carriage_return = false;
    for character in script.chars() {
        if character == '\n' {
            let line_end = utf16_offset.saturating_sub(usize::from(previous_was_carriage_return));
            lines.push(GeneratedScriptLine {
                utf16_start: line_start,
                utf16_end: line_end,
            });
            utf16_offset += 1;
            line_start = utf16_offset;
            previous_was_carriage_return = false;
        } else {
            utf16_offset += character.len_utf16();
            previous_was_carriage_return = character == '\r';
        }
    }
    lines.push(GeneratedScriptLine {
        utf16_start: line_start,
        utf16_end: utf16_offset,
    });
    lines
}

fn launch_browser(
    config: &wake_config::Test,
    options: &TestOptions,
) -> Result<BrowserDriver, TestError> {
    BrowserDriver::launch(BrowserLaunchOptions {
        executable: options.browser_path.clone(),
        headless: if options.headful {
            false
        } else {
            config.browser.headless
        },
        sandbox: config.browser.sandbox,
        viewport: Viewport {
            width: config.browser.viewport.width,
            height: config.browser.viewport.height,
            device_scale_factor: config.browser.viewport.device_scale_factor,
        },
        locale: config.browser.locale.clone(),
        timezone: config.browser.timezone.clone(),
        color_scheme: match config.browser.color_scheme {
            wake_config::TestColorScheme::Light => "light",
            wake_config::TestColorScheme::Dark => "dark",
        }
        .to_string(),
        ..BrowserLaunchOptions::default()
    })
    .map_err(|error| {
        let mut failure = TestError::new("WAKE_TEST_BROWSER", error.message);
        failure.path = error.path;
        failure
    })
}

fn parse_scheduler_plan(json: &str) -> Result<RuntimeSchedulerPlan, String> {
    let plan = serde_json::from_str::<RuntimeSchedulerPlan>(json)
        .map_err(|error| format!("Wake scheduler returned an invalid plan: {error}"))?;
    if plan.schema_version != RUNTIME_SCHEDULER_SCHEMA {
        return Err(format!(
            "Wake scheduler returned unsupported plan schema {:?}",
            plan.schema_version
        ));
    }
    for (index, case) in plan.cases.iter().enumerate() {
        if case.index != index
            || case.id.is_empty()
            || case.name.is_empty()
            || case.full_name.is_empty()
        {
            return Err(format!(
                "Wake scheduler case {index} has an invalid identity/index"
            ));
        }
        if !matches!(case.status.as_str(), "run" | "skipped" | "todo") {
            return Err(format!(
                "Wake scheduler case {} has unsupported status {:?}",
                case.id, case.status
            ));
        }
    }
    Ok(plan)
}

fn parse_scheduler_cursor(json: &str) -> Result<RuntimeSchedulerCursor, String> {
    let cursor = serde_json::from_str::<RuntimeSchedulerCursor>(json)
        .map_err(|error| format!("Wake scheduler returned an invalid cursor: {error}"))?;
    if cursor.schema_version != RUNTIME_SCHEDULER_SCHEMA {
        return Err(format!(
            "Wake scheduler returned unsupported cursor schema {:?}",
            cursor.schema_version
        ));
    }
    match cursor.status.as_str() {
        "step"
            if cursor.step.is_some()
                && cursor.partial_result.is_some()
                && cursor.result.is_none() =>
        {
            let step = cursor.step.as_ref().expect("step cursor was checked above");
            if step.id.is_empty()
                || step.suite_id.is_empty()
                || step.timeout_ms == 0
                || !matches!(
                    step.kind.as_str(),
                    "beforeAll"
                        | "beforeEach"
                        | "test"
                        | "afterEach"
                        | "cleanup"
                        | "afterAll"
                        | "finalize"
                )
                || (step.case_index.is_some()
                    != (step.case_name.is_some() && step.case_full_name.is_some()))
            {
                return Err(format!(
                    "Wake scheduler returned an invalid step descriptor {:?}",
                    step.id
                ));
            }
        }
        "complete"
            if cursor.step.is_none()
                && cursor.partial_result.is_none()
                && cursor.result.is_some() => {}
        _ => {
            return Err(format!(
                "Wake scheduler cursor has inconsistent status {:?}",
                cursor.status
            ));
        }
    }
    Ok(cursor)
}

fn parse_scheduler_ack(json: &str, expected_step_id: &str) -> Result<bool, String> {
    let ack = serde_json::from_str::<RuntimeSchedulerStepAck>(json).map_err(|error| {
        format!("Wake scheduler returned an invalid step acknowledgement: {error}")
    })?;
    if ack.schema_version != RUNTIME_SCHEDULER_SCHEMA || ack.step_id != expected_step_id {
        return Err(format!(
            "Wake scheduler acknowledged {:?} with schema {:?}, expected {expected_step_id:?}",
            ack.step_id, ack.schema_version
        ));
    }
    Ok(ack.timed_out)
}

fn scheduler_action_in_dom(
    runtime: &mut JsRuntime,
    action: &str,
    result_expression: &str,
    timeout_ms: u64,
) -> Result<String, RuntimeError> {
    runtime.set_execution_timeout(std::time::Duration::from_millis(timeout_ms.max(1)));
    runtime.execute_commonjs_and_read(
        Path::new("__wake_scheduler_action.js"),
        "",
        "",
        action,
        result_expression,
    )
}

fn next_dom_scheduler_cursor(
    runtime: &mut JsRuntime,
    timeout_ms: u64,
) -> Result<RuntimeSchedulerCursor, String> {
    let json = scheduler_action_in_dom(
        runtime,
        "globalThis.__wakeSchedulerNext();",
        "globalThis.__wakeSerializedSchedulerCursor",
        timeout_ms,
    )
    .map_err(|error| error.to_string())?;
    parse_scheduler_cursor(&json)
}

fn execute_dom_scheduler(
    runtime: &mut JsRuntime,
    plan_json: &str,
    default_timeout_ms: u64,
    cancellation: &TestCancellationToken,
) -> Result<String, String> {
    let _plan = parse_scheduler_plan(plan_json)?;
    let mut cursor = next_dom_scheduler_cursor(runtime, default_timeout_ms)?;
    loop {
        if cursor.status == "complete" {
            return serde_json::to_string(
                cursor
                    .result
                    .as_ref()
                    .expect("validated complete scheduler cursor has a result"),
            )
            .map_err(|error| format!("Wake scheduler result could not be encoded: {error}"));
        }
        let step = cursor
            .step
            .as_ref()
            .expect("validated step scheduler cursor has a step")
            .clone();
        if cancellation.is_cancelled() {
            return Err("Wake test was cancelled".to_string());
        }
        let step_id = serde_json::to_string(&step.id)
            .expect("scheduler step identifier has an infallible JSON representation");
        let action = format!(
            "globalThis.__wakeSchedulerStepPromise = globalThis.__wakeSchedulerRunStep({step_id});"
        );
        let executed = scheduler_action_in_dom(
            runtime,
            &action,
            "globalThis.__wakeSerializedSchedulerStep",
            step.timeout_ms,
        );
        match executed {
            Ok(json) => {
                parse_scheduler_ack(&json, &step.id)?;
            }
            Err(RuntimeError::Vm(error))
                if error.is_termination() && !cancellation.is_cancelled() =>
            {
                let action = format!("globalThis.__wakeSchedulerRecordTimeout({step_id});");
                let json = scheduler_action_in_dom(
                    runtime,
                    &action,
                    "globalThis.__wakeSerializedSchedulerStep",
                    default_timeout_ms,
                )
                .map_err(|error| {
                    format!(
                        "Wake scheduler could not recover after timing out {}: {error}",
                        step.kind
                    )
                })?;
                parse_scheduler_ack(&json, &step.id)?;
            }
            Err(error) => return Err(error.to_string()),
        }
        cursor = next_dom_scheduler_cursor(runtime, default_timeout_ms)?;
    }
}

fn execute_dom_graph(
    compiled: &CompiledCommonJsGraphScript,
    coverage_enabled: bool,
    timeout_ms: u64,
    cancellation: &TestCancellationToken,
) -> Result<(Result<String, String>, Option<serde_json::Value>), TestError> {
    let mut runtime = JsRuntime::new_with_coverage(coverage_enabled);
    runtime.set_execution_timeout(std::time::Duration::from_millis(timeout_ms.max(1)));
    let _cancellation_guard = RuntimeCancellationGuard::arm(&mut runtime, cancellation);
    if coverage_enabled {
        runtime.start_precise_coverage().map_err(|error| {
            TestError::new(
                "WAKE_TEST_COVERAGE",
                format!("could not start V8 precise coverage: {error}"),
            )
        })?;
    }
    let result = runtime
        .execute_compiled_commonjs_graph_and_read(
            compiled,
            "globalThis.__wakeSerializedSchedulerPlan",
        )
        .map_err(|error| error.to_string())
        .and_then(|plan| execute_dom_scheduler(&mut runtime, &plan, timeout_ms, cancellation));
    let coverage = if coverage_enabled && !cancellation.is_cancelled() {
        Some(runtime.take_precise_coverage().map_err(|error| {
            TestError::new(
                "WAKE_TEST_COVERAGE",
                format!("could not take V8 precise coverage: {error}"),
            )
        })?)
    } else {
        None
    };
    Ok((result, coverage))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserNetworkBridgeRequest {
    url: String,
    method: String,
    headers: Vec<BrowserNetworkBridgeHeader>,
    body: Option<Vec<u8>>,
    resource_type: String,
}

impl From<&FetchRequestPaused> for BrowserNetworkBridgeRequest {
    fn from(paused: &FetchRequestPaused) -> Self {
        Self {
            url: paused.request.url.clone(),
            method: paused.request.method.clone(),
            headers: paused
                .request
                .headers
                .iter()
                .map(|(name, value)| BrowserNetworkBridgeHeader {
                    name: name.clone(),
                    value: value.clone(),
                })
                .collect(),
            body: paused
                .request
                .post_data
                .as_ref()
                .map(|body| body.as_bytes().to_vec()),
            resource_type: paused.resource_type.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserNetworkBridgeHeader {
    name: String,
    value: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum BrowserNetworkDecision {
    Continue,
    Fail {
        #[serde(rename = "errorReason")]
        error_reason: String,
        #[serde(default)]
        message: Option<String>,
    },
    Fulfill {
        status: u16,
        #[serde(rename = "statusText")]
        status_text: String,
        headers: Vec<BrowserNetworkBridgeHeader>,
        body: Vec<u8>,
    },
}

fn browser_network_bridge_expression(
    request: &BrowserNetworkBridgeRequest,
) -> Result<String, String> {
    let json = serde_json::to_string(request)
        .map_err(|error| format!("could not serialize browser network request: {error}"))?;
    let literal = serde_json::to_string(&json)
        .map_err(|error| format!("could not quote browser network request: {error}"))?;
    Ok(format!(
        "globalThis.__wakeHandleBrowserNetworkRequest(JSON.parse({literal}))"
    ))
}

fn handle_browser_network_request(
    page: &BrowserPage,
    paused: &FetchRequestPaused,
    timeout_ms: u64,
) -> Result<(), String> {
    let expression = browser_network_bridge_expression(&paused.into())?;
    let value = page
        .evaluate_with_timeout(&expression, Some(timeout_ms.max(1)))
        .map_err(|error| format!("browser network bridge evaluation failed: {error}"))?;
    let serialized = value
        .as_str()
        .ok_or_else(|| "browser network bridge result was not a JSON string".to_string())?;
    let decision = serde_json::from_str::<BrowserNetworkDecision>(serialized)
        .map_err(|error| format!("browser network bridge returned invalid JSON: {error}"))?;
    match decision {
        BrowserNetworkDecision::Continue => page
            .continue_fetch_request(&paused.request_id)
            .map_err(|error| error.to_string()),
        BrowserNetworkDecision::Fail {
            error_reason,
            message,
        } => {
            let _ = message;
            page.fail_fetch_request(&paused.request_id, &error_reason)
                .map_err(|error| error.to_string())
        }
        BrowserNetworkDecision::Fulfill {
            status,
            status_text,
            headers,
            body,
        } => {
            let mut response = FetchFulfillResponse::new(status, body);
            response.headers = headers
                .into_iter()
                .map(|header| FetchHeader {
                    name: header.name,
                    value: header.value,
                })
                .collect();
            response.response_phrase = (!status_text.is_empty()).then_some(status_text);
            page.fulfill_fetch_request(&paused.request_id, &response)
                .map_err(|error| error.to_string())
        }
    }
}

fn service_browser_network(
    page: Arc<BrowserPage>,
    cancellation: BrowserCancellationToken,
    timeout_ms: u64,
) -> Result<(), String> {
    let mut first_error = None;
    loop {
        let paused = page
            .wait_for_fetch_request(std::time::Duration::from_millis(50), &cancellation)
            .map_err(|error| error.to_string())?;
        match paused {
            CdpEventWait::Event(paused) => {
                if cancellation.is_cancelled() {
                    return first_error.map_or(Ok(()), Err);
                }
                if first_error.is_some() {
                    let _ = page.fail_fetch_request(&paused.request_id, "Failed");
                    continue;
                }
                if let Err(error) = handle_browser_network_request(&page, &paused, timeout_ms) {
                    if cancellation.is_cancelled() {
                        return Ok(());
                    }
                    let failure = match page.fail_fetch_request(&paused.request_id, "Failed") {
                        Ok(()) => error,
                        Err(resume_error) => {
                            format!("{error}; could not fail paused request: {resume_error}")
                        }
                    };
                    first_error = Some(failure);
                }
            }
            CdpEventWait::TimedOut => {}
            CdpEventWait::Cancelled => return first_error.map_or(Ok(()), Err),
        }
    }
}

struct BrowserNetworkInterception {
    page: Arc<BrowserPage>,
    cancellation: BrowserCancellationToken,
    worker: Option<std::thread::JoinHandle<Result<(), String>>>,
    enabled: bool,
}

impl BrowserNetworkInterception {
    fn start(page: Arc<BrowserPage>, timeout_ms: u64) -> Result<Self, String> {
        page.enable_network_interception()
            .map_err(|error| format!("could not enable browser network interception: {error}"))?;
        let cancellation = BrowserCancellationToken::new();
        let worker_page = Arc::clone(&page);
        let worker_cancellation = cancellation.clone();
        let worker = match std::thread::Builder::new()
            .name("wake-test-browser-network".to_string())
            .spawn(move || service_browser_network(worker_page, worker_cancellation, timeout_ms))
        {
            Ok(worker) => worker,
            Err(error) => {
                let _ = page.disable_fetch_interception();
                return Err(format!(
                    "could not start browser network interception worker: {error}"
                ));
            }
        };
        Ok(Self {
            page,
            cancellation,
            worker: Some(worker),
            enabled: true,
        })
    }

    fn finish(mut self) -> Result<(), String> {
        self.stop()
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.worker.is_none() && !self.enabled {
            return Ok(());
        }
        self.cancellation.cancel();
        let _ = self.page.command_with_timeout(
            "Runtime.terminateExecution",
            serde_json::json!({}),
            std::time::Duration::from_millis(500),
        );
        let worker_result = self.worker.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| "browser network interception worker panicked".to_string())?
        });
        let disable_result = if self.enabled {
            self.enabled = false;
            self.page
                .disable_fetch_interception()
                .map_err(|error| format!("could not disable browser network interception: {error}"))
        } else {
            Ok(())
        };
        worker_result.and(disable_result)
    }
}

impl Drop for BrowserNetworkInterception {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

const BROWSER_OPERATION_SCHEMA: &str = "wake.browser.operation.v1";
const BROWSER_OPERATION_BINDING: &str = "__wakeBrowserOperationBinding";

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserInputTarget {
    x: f64,
    y: f64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BrowserInputFile {
    name: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", deny_unknown_fields)]
enum BrowserOperationCommand {
    #[serde(rename = "click")]
    Click {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
    },
    #[serde(rename = "doubleClick")]
    DoubleClick {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
    },
    #[serde(rename = "type")]
    Type {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
        text: String,
    },
    #[serde(rename = "clear")]
    Clear {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
    },
    #[serde(rename = "keyboard")]
    Keyboard {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        text: String,
    },
    #[serde(rename = "tab")]
    Tab {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        shift: bool,
    },
    #[serde(rename = "hover")]
    Hover {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
    },
    #[serde(rename = "unhover")]
    Unhover {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
    },
    #[serde(rename = "selectOptions")]
    SelectOptions {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
        indexes: Vec<usize>,
        multiple: bool,
    },
    #[serde(rename = "upload")]
    Upload {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        target: BrowserInputTarget,
        selector: String,
        files: Vec<BrowserInputFile>,
    },
    #[serde(rename = "screenshot")]
    Screenshot {
        #[serde(rename = "schemaVersion")]
        schema_version: String,
        id: String,
        key: String,
        #[serde(rename = "testFullName")]
        test_full_name: String,
        clip: Option<ScreenshotClip>,
    },
}

impl BrowserOperationCommand {
    fn schema_version(&self) -> &str {
        match self {
            Self::Click { schema_version, .. }
            | Self::DoubleClick { schema_version, .. }
            | Self::Type { schema_version, .. }
            | Self::Clear { schema_version, .. }
            | Self::Keyboard { schema_version, .. }
            | Self::Tab { schema_version, .. }
            | Self::Hover { schema_version, .. }
            | Self::Unhover { schema_version, .. }
            | Self::SelectOptions { schema_version, .. }
            | Self::Upload { schema_version, .. }
            | Self::Screenshot { schema_version, .. } => schema_version,
        }
    }

    fn id(&self) -> &str {
        match self {
            Self::Click { id, .. }
            | Self::DoubleClick { id, .. }
            | Self::Type { id, .. }
            | Self::Clear { id, .. }
            | Self::Keyboard { id, .. }
            | Self::Tab { id, .. }
            | Self::Hover { id, .. }
            | Self::Unhover { id, .. }
            | Self::SelectOptions { id, .. }
            | Self::Upload { id, .. }
            | Self::Screenshot { id, .. } => id,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowserOperationCompletion<'a> {
    schema_version: &'static str,
    id: &'a str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<&'a serde_json::Value>,
}

#[derive(Debug)]
struct BrowserOperationFailure {
    code: &'static str,
    message: String,
}

impl BrowserOperationFailure {
    fn browser(message: impl Into<String>) -> Self {
        Self {
            code: "WAKE_TEST_BROWSER",
            message: message.into(),
        }
    }

    fn snapshot(message: impl Into<String>) -> Self {
        Self {
            code: "WAKE_TEST_SNAPSHOT",
            message: message.into(),
        }
    }
}

fn browser_operation_completion_expression(
    id: &str,
    result: &Result<Option<serde_json::Value>, BrowserOperationFailure>,
) -> Result<String, String> {
    let completion = BrowserOperationCompletion {
        schema_version: BROWSER_OPERATION_SCHEMA,
        id,
        ok: result.is_ok(),
        message: result.as_ref().err().map(|error| error.message.as_str()),
        code: result.as_ref().err().map(|error| error.code),
        value: result.as_ref().ok().and_then(Option::as_ref),
    };
    let json = serde_json::to_string(&completion)
        .map_err(|error| format!("could not serialize browser operation completion: {error}"))?;
    let literal = serde_json::to_string(&json)
        .map_err(|error| format!("could not quote browser operation completion: {error}"))?;
    Ok(format!(
        "globalThis.__wakeCompleteBrowserOperation(JSON.parse({literal}))"
    ))
}

fn validate_browser_input_target(target: BrowserInputTarget) -> Result<(f64, f64), String> {
    if !target.x.is_finite() || !target.y.is_finite() {
        return Err("browser input target coordinates must be finite".to_string());
    }
    Ok((target.x, target.y))
}

fn named_keyboard_input(key: &str, modifiers: InputModifiers) -> Option<KeyboardInput> {
    let (code, virtual_key) = match key {
        "Tab" => ("Tab", 9),
        "Enter" => ("Enter", 13),
        "Backspace" => ("Backspace", 8),
        "Escape" => ("Escape", 27),
        "Delete" => ("Delete", 46),
        "Home" => ("Home", 36),
        "End" => ("End", 35),
        "ArrowLeft" => ("ArrowLeft", 37),
        "ArrowUp" => ("ArrowUp", 38),
        "ArrowRight" => ("ArrowRight", 39),
        "ArrowDown" => ("ArrowDown", 40),
        _ => return None,
    };
    let mut input = KeyboardInput::new(key, code);
    input.windows_virtual_key_code = virtual_key;
    input.native_virtual_key_code = virtual_key;
    input.modifiers = modifiers;
    Some(input)
}

fn character_keyboard_input(character: char) -> Option<KeyboardInput> {
    let mut input = if character.is_ascii_alphabetic() {
        let uppercase = character.to_ascii_uppercase();
        let mut input = KeyboardInput::new(character.to_string(), format!("Key{uppercase}"));
        input.windows_virtual_key_code = uppercase as u32;
        input.native_virtual_key_code = uppercase as u32;
        if character.is_ascii_uppercase() {
            input.modifiers = InputModifiers::SHIFT;
        }
        input
    } else if character.is_ascii_digit() {
        let mut input = KeyboardInput::new(character.to_string(), format!("Digit{character}"));
        input.windows_virtual_key_code = character as u32;
        input.native_virtual_key_code = character as u32;
        input
    } else if character == ' ' {
        let mut input = KeyboardInput::new(" ", "Space");
        input.windows_virtual_key_code = 32;
        input.native_virtual_key_code = 32;
        input
    } else {
        return None;
    };
    input.text = Some(character.to_string());
    input.unmodified_text = Some(character.to_string());
    Some(input)
}

fn dispatch_keyboard_sequence(page: &BrowserPage, text: &str) -> Result<(), String> {
    let mut remaining = text;
    while !remaining.is_empty() {
        if let Some(token_end) = remaining
            .strip_prefix('{')
            .and_then(|value| value.find('}').map(|index| index + 1))
        {
            let token = &remaining[1..token_end];
            if let Some(input) = named_keyboard_input(token, InputModifiers::NONE) {
                page.press_key(&input).map_err(|error| error.to_string())?;
                remaining = &remaining[token_end + 1..];
                continue;
            }
        }
        let character = remaining
            .chars()
            .next()
            .expect("non-empty keyboard sequence has a character");
        if let Some(input) = character_keyboard_input(character) {
            page.press_key(&input).map_err(|error| error.to_string())?;
        } else {
            page.insert_text(&character.to_string())
                .map_err(|error| error.to_string())?;
        }
        remaining = &remaining[character.len_utf8()..];
    }
    Ok(())
}

fn pointer_click(page: &BrowserPage, target: BrowserInputTarget) -> Result<(), String> {
    let (x, y) = validate_browser_input_target(target)?;
    page.pointer_move(x, y)
        .and_then(|()| page.pointer_click(x, y, PointerButton::Left, InputModifiers::NONE))
        .map_err(|error| error.to_string())
}

fn pointer_double_click(page: &BrowserPage, target: BrowserInputTarget) -> Result<(), String> {
    let (x, y) = validate_browser_input_target(target)?;
    page.pointer_move(x, y).map_err(|error| error.to_string())?;
    for click_count in [1, 2] {
        let mut down = PointerInput::at(x, y);
        down.button = PointerButton::Left;
        down.buttons = 1;
        down.click_count = click_count;
        page.dispatch_pointer_event(PointerEventType::Down, &down)
            .map_err(|error| error.to_string())?;
        let mut up = PointerInput::at(x, y);
        up.button = PointerButton::Left;
        up.click_count = click_count;
        page.dispatch_pointer_event(PointerEventType::Up, &up)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn write_browser_input_files(
    root: &Path,
    upload_sequence: u64,
    files: &[BrowserInputFile],
) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::with_capacity(files.len());
    for (index, file) in files.iter().enumerate() {
        let file_name = Path::new(&file.name)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty() && *name != "." && *name != "..")
            .ok_or_else(|| "browser upload file name is invalid".to_string())?;
        let directory = root
            .join(upload_sequence.to_string())
            .join(index.to_string());
        fs::create_dir_all(&directory)
            .map_err(|error| format!("could not create browser upload directory: {error}"))?;
        let path = directory.join(file_name);
        fs::write(&path, &file.bytes)
            .map_err(|error| format!("could not write browser upload file: {error}"))?;
        paths.push(path);
    }
    Ok(paths)
}

fn handle_browser_input_command(
    page: &BrowserPage,
    command: &BrowserOperationCommand,
    upload_root: &Path,
    upload_sequence: &mut u64,
) -> Result<(), String> {
    if command.schema_version() != BROWSER_OPERATION_SCHEMA {
        return Err(format!(
            "unsupported browser operation schema {:?}",
            command.schema_version()
        ));
    }
    match command {
        BrowserOperationCommand::Click { target, .. } => pointer_click(page, *target),
        BrowserOperationCommand::DoubleClick { target, .. } => pointer_double_click(page, *target),
        BrowserOperationCommand::Type { target, text, .. } => {
            pointer_click(page, *target)?;
            dispatch_keyboard_sequence(page, text)
        }
        BrowserOperationCommand::Clear { target, .. } => {
            pointer_click(page, *target)?;
            let mut select_all = KeyboardInput::new("a", "KeyA");
            select_all.windows_virtual_key_code = 65;
            select_all.native_virtual_key_code = 65;
            select_all.modifiers = InputModifiers::CONTROL;
            select_all.commands.push("selectAll".to_string());
            page.press_key(&select_all)
                .map_err(|error| error.to_string())?;
            let backspace = named_keyboard_input("Backspace", InputModifiers::NONE)
                .expect("Backspace is a supported named key");
            page.press_key(&backspace)
                .map_err(|error| error.to_string())
        }
        BrowserOperationCommand::Keyboard { text, .. } => dispatch_keyboard_sequence(page, text),
        BrowserOperationCommand::Tab { shift, .. } => {
            let modifiers = if *shift {
                InputModifiers::SHIFT
            } else {
                InputModifiers::NONE
            };
            let tab = named_keyboard_input("Tab", modifiers).expect("Tab is a supported named key");
            page.press_key(&tab).map_err(|error| error.to_string())
        }
        BrowserOperationCommand::Hover { target, .. } => {
            let (x, y) = validate_browser_input_target(*target)?;
            page.pointer_move(x, y).map_err(|error| error.to_string())
        }
        BrowserOperationCommand::Unhover { target, .. } => {
            validate_browser_input_target(*target)?;
            page.pointer_move(-1.0, -1.0)
                .map_err(|error| error.to_string())
        }
        BrowserOperationCommand::SelectOptions {
            target,
            indexes,
            multiple,
            ..
        } => {
            pointer_click(page, *target)?;
            if *multiple {
                return Ok(());
            }
            let index = indexes
                .first()
                .copied()
                .ok_or_else(|| "selectOptions requires one option index".to_string())?;
            for key in std::iter::once("Home")
                .chain(std::iter::repeat_n("ArrowDown", index))
                .chain(std::iter::once("Enter"))
            {
                let input = named_keyboard_input(key, InputModifiers::NONE)
                    .expect("selectOptions uses supported named keys");
                page.press_key(&input).map_err(|error| error.to_string())?;
            }
            Ok(())
        }
        BrowserOperationCommand::Upload {
            target,
            selector,
            files,
            ..
        } => {
            let (x, y) = validate_browser_input_target(*target)?;
            page.pointer_move(x, y).map_err(|error| error.to_string())?;
            *upload_sequence = upload_sequence
                .checked_add(1)
                .ok_or_else(|| "browser upload identifier space was exhausted".to_string())?;
            let paths = write_browser_input_files(upload_root, *upload_sequence, files.as_slice())?;
            page.set_file_input_files(selector, &paths)
                .map_err(|error| error.to_string())
        }
        BrowserOperationCommand::Screenshot { .. } => {
            Err("screenshot command reached the input handler".to_string())
        }
    }
}

#[derive(Debug, Default)]
struct BrowserOperationOutput {
    snapshot: SnapshotSummary,
    artifacts: Vec<TestArtifact>,
    failures: Vec<String>,
}

struct BrowserScreenshotPolicy {
    directory: PathBuf,
    difference_directory: PathBuf,
    suite_id: String,
    suite_path: String,
    baseline_prefix: String,
    profile_hash: String,
    update: SnapshotUpdate,
    seen: BTreeSet<String>,
}

fn browser_screenshot_profile(
    os: &str,
    arch: &str,
    browser: BrowserKind,
    browser_version: &str,
    options: &BrowserLaunchOptions,
) -> String {
    let browser_major = browser_version
        .split(|character: char| !character.is_ascii_digit())
        .find(|part| !part.is_empty())
        .unwrap_or("unknown");
    serde_json::json!({
        "schema": "wake.screenshot.profile.v1",
        "os": os,
        "arch": arch,
        "browser": browser,
        "browserMajor": browser_major,
        "headless": options.headless,
        "viewport": options.viewport,
        "locale": options.locale,
        "timezone": options.timezone,
        "colorScheme": options.color_scheme,
        "reducedMotion": REDUCED_MOTION,
    })
    .to_string()
}

impl BrowserScreenshotPolicy {
    fn new(
        suite_path: &Path,
        suite_identity_label: &str,
        directory: &str,
        update: SnapshotUpdate,
        browser: &BrowserDriver,
    ) -> Self {
        let directory = suite_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(directory);
        let difference_directory = directory.join("__diffs__");
        let suite_id = stable_id("suite", suite_identity_label);
        let suite_hash = stable_id("screenshot-suite", suite_identity_label);
        let options = browser.launch_options();
        let profile = browser_screenshot_profile(
            std::env::consts::OS,
            std::env::consts::ARCH,
            browser.installation.kind,
            &browser.installation.version,
            options,
        );
        let profile_hash = stable_id("screenshot-profile", &profile);
        let baseline_prefix = format!("wake-v1--{suite_hash}--{profile_hash}--");
        Self {
            directory,
            difference_directory,
            suite_id,
            suite_path: suite_identity_label.to_string(),
            baseline_prefix,
            profile_hash,
            update,
            seen: BTreeSet::new(),
        }
    }

    fn baseline_file_name(&self, key: &str) -> String {
        format!(
            "{}{}--{}.png",
            self.baseline_prefix,
            screenshot_slug(key),
            stable_id("snapshot", key)
        )
    }

    fn artifact(
        &self,
        kind: &str,
        path: &Path,
        key: &str,
        test_full_name: &str,
        status: &str,
    ) -> TestArtifact {
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "profileHash".to_string(),
            serde_json::Value::String(self.profile_hash.clone()),
        );
        metadata.insert(
            "reducedMotion".to_string(),
            serde_json::Value::String(REDUCED_MOTION.to_string()),
        );
        metadata.insert(
            "snapshotKey".to_string(),
            serde_json::Value::String(key.to_string()),
        );
        metadata.insert(
            "status".to_string(),
            serde_json::Value::String(status.to_string()),
        );
        let path_label = normalize_path(path);
        TestArtifact {
            id: stable_id("artifact", &format!("{kind}:{path_label}")),
            kind: kind.to_string(),
            path: path_label,
            suite_id: Some(self.suite_id.clone()),
            test_id: Some(stable_id(
                "test",
                &format!("{}:{test_full_name}", self.suite_path),
            )),
            metadata,
        }
    }

    fn difference_paths(&self, baseline_file_name: &str) -> (PathBuf, PathBuf) {
        let stem = baseline_file_name
            .strip_suffix(".png")
            .unwrap_or(baseline_file_name);
        (
            self.difference_directory
                .join(format!("{stem}.received.png")),
            self.difference_directory.join(format!("{stem}.diff.html")),
        )
    }

    fn remove_stale_difference(
        &self,
        baseline_file_name: &str,
    ) -> Result<(), BrowserOperationFailure> {
        let (received_path, diff_path) = self.difference_paths(baseline_file_name);
        for path in [received_path, diff_path] {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(BrowserOperationFailure::snapshot(format!(
                        "could not remove stale screenshot artifact {}: {error}",
                        path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    fn write_difference(
        &self,
        baseline_file_name: &str,
        expected: Option<&[u8]>,
        received: &[u8],
        key: &str,
        test_full_name: &str,
        output: &mut BrowserOperationOutput,
    ) -> Result<(), BrowserOperationFailure> {
        let (received_path, diff_path) = self.difference_paths(baseline_file_name);
        atomic_screenshot_write(&received_path, received)?;
        let expected_data = expected.map(|png| {
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(png)
            )
        });
        let received_data = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(received)
        );
        let expected_image = expected_data.as_ref().map_or_else(
            || "<p class=missing>No baseline exists for this profile.</p>".to_string(),
            |source| format!("<img alt=\"Expected screenshot\" src=\"{source}\">"),
        );
        let overlay_image = expected_data.as_ref().map_or_else(String::new, |source| {
            format!(
                "<div class=overlay><img alt=\"Expected overlay\" src=\"{source}\"><img alt=\"Received overlay\" src=\"{received_data}\"></div>"
            )
        });
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>Wake screenshot diff</title><style>body{{font:14px system-ui;margin:24px;background:#111;color:#eee}}main{{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:16px}}section{{min-width:0}}img{{display:block;max-width:100%;background:#fff}}.overlay{{display:grid}}.overlay img{{grid-area:1/1;mix-blend-mode:difference}}.missing{{padding:24px;border:1px dashed #888}}</style><h1>{}</h1><p>Profile: {}</p><main><section><h2>Expected</h2>{}</section><section><h2>Received</h2><img alt=\"Received screenshot\" src=\"{}\"></section><section><h2>Difference overlay</h2>{}</section></main>",
            html_escape(key),
            html_escape(&self.profile_hash),
            expected_image,
            received_data,
            overlay_image,
        );
        atomic_screenshot_write(&diff_path, html.as_bytes())?;
        output.artifacts.push(self.artifact(
            "screenshot-received",
            &received_path,
            key,
            test_full_name,
            "unmatched",
        ));
        output.artifacts.push(self.artifact(
            "screenshot-diff",
            &diff_path,
            key,
            test_full_name,
            "unmatched",
        ));
        Ok(())
    }

    fn capture(
        &mut self,
        page: &BrowserPage,
        key: &str,
        test_full_name: &str,
        clip: Option<&ScreenshotClip>,
        output: &mut BrowserOperationOutput,
    ) -> Result<serde_json::Value, BrowserOperationFailure> {
        if !key.is_empty() && key.len() <= 4096 {
            self.seen.insert(self.baseline_file_name(key));
        }
        let received = page
            .screenshot_png_with_clip(clip)
            .map_err(|error| BrowserOperationFailure::browser(error.to_string()))?;
        self.compare_png(key, test_full_name, &received, output)
    }

    fn compare_png(
        &mut self,
        key: &str,
        test_full_name: &str,
        received: &[u8],
        output: &mut BrowserOperationOutput,
    ) -> Result<serde_json::Value, BrowserOperationFailure> {
        if key.is_empty()
            || key.len() > 4096
            || test_full_name.is_empty()
            || test_full_name.len() > 4096
        {
            return Err(BrowserOperationFailure::snapshot(
                "screenshot key/test name is empty or exceeds 4096 bytes",
            ));
        }
        let baseline_file_name = self.baseline_file_name(key);
        self.seen.insert(baseline_file_name.clone());
        let baseline_path = self.directory.join(&baseline_file_name);
        let expected = match fs::read(&baseline_path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(BrowserOperationFailure::snapshot(format!(
                    "could not read screenshot baseline {}: {error}",
                    baseline_path.display()
                )));
            }
        };
        let response = match expected {
            Some(expected) if expected == received => {
                output.snapshot.matched += 1;
                self.remove_stale_difference(&baseline_file_name)?;
                output.artifacts.push(self.artifact(
                    "screenshot-baseline",
                    &baseline_path,
                    key,
                    test_full_name,
                    "matched",
                ));
                serde_json::json!({"pass": true, "message": ""})
            }
            Some(_) if self.update == SnapshotUpdate::All => {
                atomic_screenshot_write(&baseline_path, received)?;
                self.remove_stale_difference(&baseline_file_name)?;
                output.snapshot.updated += 1;
                output.artifacts.push(self.artifact(
                    "screenshot-baseline",
                    &baseline_path,
                    key,
                    test_full_name,
                    "updated",
                ));
                serde_json::json!({"pass": true, "message": ""})
            }
            Some(expected) => {
                output.snapshot.unmatched += 1;
                output.artifacts.push(self.artifact(
                    "screenshot-baseline",
                    &baseline_path,
                    key,
                    test_full_name,
                    "unmatched",
                ));
                self.write_difference(
                    &baseline_file_name,
                    Some(&expected),
                    received,
                    key,
                    test_full_name,
                    output,
                )?;
                let (received_path, diff_path) = self.difference_paths(&baseline_file_name);
                serde_json::json!({
                    "pass": false,
                    "code": "WAKE_TEST_SNAPSHOT",
                    "message": format!("Screenshot {key} did not match profile {}", self.profile_hash),
                    "diff": {
                        "expected": normalize_path(&baseline_path),
                        "received": normalize_path(&received_path),
                        "unified": format!("Visual diff: {}", normalize_path(&diff_path)),
                    },
                })
            }
            None if self.update != SnapshotUpdate::None => {
                atomic_screenshot_write(&baseline_path, received)?;
                self.remove_stale_difference(&baseline_file_name)?;
                output.snapshot.added += 1;
                output.artifacts.push(self.artifact(
                    "screenshot-baseline",
                    &baseline_path,
                    key,
                    test_full_name,
                    "added",
                ));
                serde_json::json!({"pass": true, "message": ""})
            }
            None => {
                output.snapshot.unmatched += 1;
                self.write_difference(
                    &baseline_file_name,
                    None,
                    received,
                    key,
                    test_full_name,
                    output,
                )?;
                let (received_path, diff_path) = self.difference_paths(&baseline_file_name);
                serde_json::json!({
                    "pass": false,
                    "code": "WAKE_TEST_SNAPSHOT",
                    "message": format!("Screenshot {key} has no baseline for profile {}", self.profile_hash),
                    "diff": {
                        "expected": serde_json::Value::Null,
                        "received": normalize_path(&received_path),
                        "unified": format!("Visual diff: {}", normalize_path(&diff_path)),
                    },
                })
            }
        };
        Ok(response)
    }

    fn finish(
        &mut self,
        output: &mut BrowserOperationOutput,
    ) -> Result<(), BrowserOperationFailure> {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(BrowserOperationFailure::snapshot(format!(
                    "could not scan screenshot baselines {}: {error}",
                    self.directory.display()
                )));
            }
        };
        let mut obsolete = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| {
                BrowserOperationFailure::snapshot(format!(
                    "could not inspect screenshot baseline directory: {error}"
                ))
            })?;
            if !entry
                .file_type()
                .map_err(|error| BrowserOperationFailure::snapshot(error.to_string()))?
                .is_file()
            {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&self.baseline_prefix)
                && name.ends_with(".png")
                && !self.seen.contains(&name)
            {
                obsolete.push((name, entry.path()));
            }
        }
        obsolete.sort_by(|left, right| left.0.cmp(&right.0));
        if self.update == SnapshotUpdate::All {
            for (name, path) in &obsolete {
                fs::remove_file(path).map_err(|error| {
                    BrowserOperationFailure::snapshot(format!(
                        "could not remove obsolete screenshot baseline {}: {error}",
                        path.display()
                    ))
                })?;
                self.remove_stale_difference(name)?;
            }
            output.snapshot.files_removed += obsolete.len();
        } else {
            output.snapshot.obsolete += obsolete.len();
            output.failures.extend(
                obsolete
                    .into_iter()
                    .map(|(_, path)| format!("Obsolete screenshot baseline: {}", path.display())),
            );
        }
        Ok(())
    }
}

fn screenshot_slug(key: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in key.chars() {
        if slug.len() >= 48 {
            break;
        }
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator && !slug.is_empty() {
            slug.push('-');
            separator = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "screenshot".to_string()
    } else {
        slug
    }
}

fn atomic_screenshot_write(path: &Path, contents: &[u8]) -> Result<(), BrowserOperationFailure> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        BrowserOperationFailure::snapshot(format!(
            "could not create screenshot directory {}: {error}",
            parent.display()
        ))
    })?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".wake-screenshot-")
        .tempfile_in(parent)
        .map_err(|error| BrowserOperationFailure::snapshot(error.to_string()))?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| BrowserOperationFailure::snapshot(error.to_string()))?;
    temporary.persist(path).map_err(|error| {
        BrowserOperationFailure::snapshot(format!(
            "could not replace screenshot file {}: {}",
            path.display(),
            error.error
        ))
    })?;
    Ok(())
}

fn parse_browser_operation_binding(event: &serde_json::Value) -> Result<Option<&str>, String> {
    let params = event
        .get("params")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "Runtime.bindingCalled omitted params".to_string())?;
    let name = params
        .get("name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Runtime.bindingCalled omitted params.name".to_string())?;
    if name != BROWSER_OPERATION_BINDING {
        return Ok(None);
    }
    params
        .get("payload")
        .and_then(serde_json::Value::as_str)
        .map(Some)
        .ok_or_else(|| "Runtime.bindingCalled omitted params.payload".to_string())
}

fn handle_browser_operation(
    page: &BrowserPage,
    command: &BrowserOperationCommand,
    upload_root: &Path,
    upload_sequence: &mut u64,
    screenshot_policy: &mut BrowserScreenshotPolicy,
    output: &mut BrowserOperationOutput,
) -> Result<Option<serde_json::Value>, BrowserOperationFailure> {
    if command.schema_version() != BROWSER_OPERATION_SCHEMA {
        return Err(BrowserOperationFailure::browser(format!(
            "unsupported browser operation schema {:?}",
            command.schema_version()
        )));
    }
    if let BrowserOperationCommand::Screenshot {
        key,
        test_full_name,
        clip,
        ..
    } = command
    {
        return screenshot_policy
            .capture(page, key, test_full_name, clip.as_ref(), output)
            .map(Some);
    }
    handle_browser_input_command(page, command, upload_root, upload_sequence)
        .map(|()| None)
        .map_err(BrowserOperationFailure::browser)
}

fn service_browser_operations(
    page: Arc<BrowserPage>,
    cancellation: BrowserCancellationToken,
    timeout_ms: u64,
    upload_directory: tempfile::TempDir,
    mut screenshot_policy: BrowserScreenshotPolicy,
    finalize_screenshots: Arc<AtomicBool>,
) -> Result<BrowserOperationOutput, String> {
    let mut upload_sequence = 0;
    let mut output = BrowserOperationOutput::default();
    loop {
        match page
            .wait_for_event(
                "Runtime.bindingCalled",
                std::time::Duration::from_millis(50),
                &cancellation,
            )
            .map_err(|error| error.to_string())?
        {
            CdpEventWait::Event(event) => {
                if cancellation.is_cancelled() {
                    if finalize_screenshots.load(Ordering::Acquire) {
                        screenshot_policy
                            .finish(&mut output)
                            .map_err(|error| format!("{}: {}", error.code, error.message))?;
                    }
                    return Ok(output);
                }
                let Some(payload) = parse_browser_operation_binding(&event)? else {
                    continue;
                };
                let command =
                    serde_json::from_str::<BrowserOperationCommand>(payload).map_err(|error| {
                        format!("browser operation binding payload is invalid: {error}")
                    })?;
                if command.id().is_empty() {
                    return Err("browser operation command id must not be empty".to_string());
                }
                let result = handle_browser_operation(
                    &page,
                    &command,
                    upload_directory.path(),
                    &mut upload_sequence,
                    &mut screenshot_policy,
                    &mut output,
                );
                let expression = browser_operation_completion_expression(command.id(), &result)?;
                page.evaluate_with_timeout(&expression, Some(timeout_ms.max(1)))
                    .map_err(|error| {
                        format!("browser operation completion evaluation failed: {error}")
                    })?;
            }
            CdpEventWait::TimedOut => {}
            CdpEventWait::Cancelled => {
                if finalize_screenshots.load(Ordering::Acquire) {
                    screenshot_policy
                        .finish(&mut output)
                        .map_err(|error| format!("{}: {}", error.code, error.message))?;
                }
                return Ok(output);
            }
        }
    }
}

struct BrowserOperationDispatcher {
    page: Arc<BrowserPage>,
    cancellation: BrowserCancellationToken,
    worker: Option<std::thread::JoinHandle<Result<BrowserOperationOutput, String>>>,
    binding_added: bool,
    output: Option<BrowserOperationOutput>,
    finalize_screenshots: Arc<AtomicBool>,
}

impl BrowserOperationDispatcher {
    fn start(
        page: Arc<BrowserPage>,
        timeout_ms: u64,
        screenshot_policy: BrowserScreenshotPolicy,
    ) -> Result<Self, String> {
        page.command(
            "Runtime.addBinding",
            serde_json::json!({"name": BROWSER_OPERATION_BINDING}),
        )
        .map_err(|error| format!("could not install browser operation binding: {error}"))?;
        let upload_directory = tempfile::Builder::new()
            .prefix("wake-browser-upload-")
            .tempdir()
            .map_err(|error| format!("could not create browser upload directory: {error}"));
        let upload_directory = match upload_directory {
            Ok(directory) => directory,
            Err(error) => {
                let _ = page.command(
                    "Runtime.removeBinding",
                    serde_json::json!({"name": BROWSER_OPERATION_BINDING}),
                );
                return Err(error);
            }
        };
        let cancellation = BrowserCancellationToken::new();
        let worker_page = Arc::clone(&page);
        let worker_cancellation = cancellation.clone();
        let finalize_screenshots = Arc::new(AtomicBool::new(false));
        let worker_finalize_screenshots = Arc::clone(&finalize_screenshots);
        let worker = match std::thread::Builder::new()
            .name("wake-test-browser-operations".to_string())
            .spawn(move || {
                service_browser_operations(
                    worker_page,
                    worker_cancellation,
                    timeout_ms,
                    upload_directory,
                    screenshot_policy,
                    worker_finalize_screenshots,
                )
            }) {
            Ok(worker) => worker,
            Err(error) => {
                let _ = page.command(
                    "Runtime.removeBinding",
                    serde_json::json!({"name": BROWSER_OPERATION_BINDING}),
                );
                return Err(format!("could not start browser operation worker: {error}"));
            }
        };
        Ok(Self {
            page,
            cancellation,
            worker: Some(worker),
            binding_added: true,
            output: None,
            finalize_screenshots,
        })
    }

    fn finish(mut self, reconcile_screenshots: bool) -> Result<BrowserOperationOutput, String> {
        self.finalize_screenshots
            .store(reconcile_screenshots, Ordering::Release);
        self.stop()?;
        Ok(self.output.take().unwrap_or_default())
    }

    fn stop(&mut self) -> Result<(), String> {
        if self.worker.is_none() && !self.binding_added {
            return Ok(());
        }
        self.cancellation.cancel();
        let worker_result = self.worker.take().map_or(Ok(None), |worker| {
            worker
                .join()
                .map_err(|_| "browser operation worker panicked".to_string())?
                .map(Some)
        });
        let remove_result = if self.binding_added {
            self.binding_added = false;
            self.page
                .command(
                    "Runtime.removeBinding",
                    serde_json::json!({"name": BROWSER_OPERATION_BINDING}),
                )
                .map(|_| ())
                .map_err(|error| format!("could not remove browser operation binding: {error}"))
        } else {
            Ok(())
        };
        match worker_result {
            Ok(Some(output)) => self.output = Some(output),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        remove_result
    }
}

impl Drop for BrowserOperationDispatcher {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

type BrowserGraphExecution = (
    Result<String, String>,
    Option<serde_json::Value>,
    BrowserOperationOutput,
);

const BROWSER_STEP_TRANSPORT_GRACE_MS: u64 = 1_000;

#[derive(Debug)]
enum BrowserSchedulerError {
    Browser(BrowserError),
    Protocol(String),
    Watchdog(String),
}

impl fmt::Display for BrowserSchedulerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Browser(error) => error.fmt(formatter),
            Self::Protocol(message) | Self::Watchdog(message) => formatter.write_str(message),
        }
    }
}

impl From<BrowserError> for BrowserSchedulerError {
    fn from(error: BrowserError) -> Self {
        Self::Browser(error)
    }
}

#[derive(Debug)]
enum BrowserGraphError {
    Cancelled,
    Infrastructure(BrowserSchedulerError),
    Runtime(String),
}

impl From<BrowserSchedulerError> for BrowserGraphError {
    fn from(error: BrowserSchedulerError) -> Self {
        Self::Infrastructure(error)
    }
}

fn browser_infrastructure_error(error: BrowserSchedulerError, path: &Path) -> TestError {
    TestError::new("WAKE_TEST_BROWSER", error.to_string()).at(path)
}

enum BrowserStepEvaluation {
    Completed(String),
    DeadlineExceeded,
}

fn browser_evaluate_string(
    page: &BrowserPage,
    expression: &str,
    timeout_ms: u64,
) -> Result<String, BrowserSchedulerError> {
    page.evaluate_with_timeout(expression, Some(timeout_ms.max(1)))
        .map_err(BrowserSchedulerError::Browser)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            BrowserSchedulerError::Protocol("browser scheduler result was not a string".to_string())
        })
}

fn browser_evaluate_scheduler_step(
    page: &Arc<BrowserPage>,
    expression: &str,
    timeout_ms: u64,
) -> Result<BrowserStepEvaluation, BrowserSchedulerError> {
    let timeout_ms = timeout_ms.max(1);
    let (completion, deadline) = std::sync::mpsc::sync_channel(1);
    let watchdog_page = Arc::clone(page);
    let watchdog = std::thread::Builder::new()
        .name("wake-test-browser-step-deadline".to_string())
        .spawn(
            move || match deadline.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Ok(false),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => watchdog_page
                    .terminate_execution_with_transport_timeout(BROWSER_STEP_TRANSPORT_GRACE_MS)
                    .map(|()| true),
            },
        )
        .map_err(|error| {
            BrowserSchedulerError::Watchdog(format!(
                "could not start the browser step deadline watchdog: {error}"
            ))
        })?;
    let evaluated = page.evaluate_with_transport_timeout(
        expression,
        timeout_ms.saturating_add(BROWSER_STEP_TRANSPORT_GRACE_MS),
    );
    let _ = completion.send(());
    let deadline_exceeded = watchdog
        .join()
        .map_err(|_| {
            BrowserSchedulerError::Watchdog("browser step deadline watchdog panicked".to_string())
        })?
        .map_err(BrowserSchedulerError::Browser)?;
    if deadline_exceeded {
        return Ok(BrowserStepEvaluation::DeadlineExceeded);
    }
    let value = evaluated.map_err(BrowserSchedulerError::Browser)?;
    let json = value.as_str().map(str::to_string).ok_or_else(|| {
        BrowserSchedulerError::Protocol(
            "browser scheduler step acknowledgement was not a string".to_string(),
        )
    })?;
    Ok(BrowserStepEvaluation::Completed(json))
}

fn next_browser_scheduler_cursor(
    page: &BrowserPage,
    timeout_ms: u64,
) -> Result<RuntimeSchedulerCursor, BrowserSchedulerError> {
    let json = browser_evaluate_string(page, "globalThis.__wakeSchedulerNext()", timeout_ms)?;
    parse_scheduler_cursor(&json).map_err(BrowserSchedulerError::Protocol)
}

fn browser_timeout_failure(step: &RuntimeSchedulerStep) -> RuntimeFailure {
    let label = if step.kind == "test" {
        "Test callback".to_string()
    } else {
        format!("{} phase", step.kind)
    };
    RuntimeFailure {
        message: format!("{label} exceeded {} ms", step.timeout_ms),
        code: Some("WAKE_TEST_TIMEOUT".to_string()),
        stack: step.registration_stack.clone(),
        diff: None,
    }
}

fn browser_timeout_result(
    plan: &RuntimeSchedulerPlan,
    cursor: &RuntimeSchedulerCursor,
    step: &RuntimeSchedulerStep,
) -> Result<RuntimeSuiteResult, String> {
    let mut result = cursor
        .partial_result
        .clone()
        .ok_or_else(|| "browser timeout cursor omitted its partial result".to_string())?;
    let failure = browser_timeout_failure(step);
    if let Some(case_index) = step.case_index {
        let planned = plan
            .cases
            .get(case_index)
            .ok_or_else(|| format!("browser timeout referenced unknown case index {case_index}"))?;
        if result.cases.len() <= case_index {
            result.cases.push(RuntimeCaseResult {
                name: planned.name.clone(),
                full_name: planned.full_name.clone(),
                status: TestStatus::Failed,
                duration_ms: step.timeout_ms,
                failures: vec![failure.clone()],
                assertions: 0,
                registration_stack: planned.registration_stack.clone(),
            });
        } else {
            let case = &mut result.cases[case_index];
            case.status = TestStatus::Failed;
            case.duration_ms = case.duration_ms.saturating_add(step.timeout_ms);
            case.failures.push(failure.clone());
        }
    } else {
        result.failures.push(failure);
    }
    for planned in plan.cases.iter().skip(result.cases.len()) {
        result.cases.push(RuntimeCaseResult {
            name: planned.name.clone(),
            full_name: planned.full_name.clone(),
            status: if planned.status == "todo" {
                TestStatus::Todo
            } else {
                TestStatus::Skipped
            },
            duration_ms: 0,
            failures: Vec::new(),
            assertions: 0,
            registration_stack: planned.registration_stack.clone(),
        });
    }
    result.status = TestStatus::Failed;
    Ok(result)
}

fn execute_browser_graph(
    browser: &BrowserDriver,
    compiled: &CompiledCommonJsGraphScript,
    coverage_enabled: bool,
    timeout_ms: u64,
    path: &Path,
    screenshot_policy: BrowserScreenshotPolicy,
    cancellation: &TestCancellationToken,
) -> Result<BrowserGraphExecution, TestError> {
    if cancellation.is_cancelled() {
        return Ok((
            Err("Wake browser test was cancelled".to_string()),
            None,
            BrowserOperationOutput::default(),
        ));
    }
    let context = browser
        .create_context()
        .map_err(|error| TestError::new("WAKE_TEST_BROWSER", error.to_string()).at(path))?;
    let page = Arc::new(
        context
            .new_page("about:blank")
            .map_err(|error| TestError::new("WAKE_TEST_BROWSER", error.to_string()).at(path))?,
    );
    let _cancellation_guard =
        BrowserExecutionCancellationGuard::arm(Arc::clone(&page), cancellation);
    let network_interception = BrowserNetworkInterception::start(Arc::clone(&page), timeout_ms)
        .map_err(|error| TestError::new("WAKE_TEST_NETWORK", error).at(path))?;
    let operation_dispatcher =
        BrowserOperationDispatcher::start(Arc::clone(&page), timeout_ms, screenshot_policy)
            .map_err(|error| TestError::new("WAKE_TEST_BROWSER", error).at(path))?;
    if coverage_enabled {
        page.start_precise_coverage()
            .map_err(|error| TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(path))?;
    }
    let mut browser_context_fatal = false;
    let graph_result = (|| -> Result<String, BrowserGraphError> {
        page.evaluate_with_timeout(compiled.script(), Some(timeout_ms.max(1)))
            .map_err(|error| BrowserGraphError::Runtime(error.to_string()))?;
        let plan_json = browser_evaluate_string(
            &page,
            "(async () => { if (globalThis.__wakeEntryModulePromise) await globalThis.__wakeEntryModulePromise; if (globalThis.__wakeSchedulerPreparationPromise) await globalThis.__wakeSchedulerPreparationPromise; return globalThis.__wakeSerializedSchedulerPlan })()",
            timeout_ms,
        )?;
        let plan = parse_scheduler_plan(&plan_json).map_err(BrowserSchedulerError::Protocol)?;
        let mut cursor = next_browser_scheduler_cursor(&page, timeout_ms)?;
        loop {
            if cursor.status == "complete" {
                return serde_json::to_string(
                    cursor
                        .result
                        .as_ref()
                        .expect("validated complete scheduler cursor has a result"),
                )
                .map_err(|error| {
                    BrowserSchedulerError::Protocol(format!(
                        "Wake scheduler result could not be encoded: {error}"
                    ))
                    .into()
                });
            }
            let step = cursor
                .step
                .as_ref()
                .expect("validated step scheduler cursor has a step")
                .clone();
            if cancellation.is_cancelled() {
                return Err(BrowserGraphError::Cancelled);
            }
            let step_id = serde_json::to_string(&step.id)
                .expect("scheduler step identifier has an infallible JSON representation");
            let expression = format!("globalThis.__wakeSchedulerRunStep({step_id})");
            let step_evaluation =
                match browser_evaluate_scheduler_step(&page, &expression, step.timeout_ms) {
                    Ok(evaluation) => evaluation,
                    Err(_) if cancellation.is_cancelled() => {
                        return Err(BrowserGraphError::Cancelled);
                    }
                    Err(error) => return Err(error.into()),
                };
            match step_evaluation {
                BrowserStepEvaluation::Completed(json) => {
                    if parse_scheduler_ack(&json, &step.id)
                        .map_err(BrowserSchedulerError::Protocol)?
                    {
                        browser_context_fatal = true;
                        page.terminate_execution_with_transport_timeout(
                            BROWSER_STEP_TRANSPORT_GRACE_MS,
                        )
                        .map_err(BrowserSchedulerError::Browser)?;
                        let partial = browser_timeout_result(&plan, &cursor, &step)
                            .map_err(BrowserSchedulerError::Protocol)?;
                        return serde_json::to_string(&partial).map_err(|encoding| {
                            BrowserSchedulerError::Protocol(format!(
                                "Wake scheduler could not encode a browser timeout result: {encoding}"
                            ))
                            .into()
                        });
                    }
                }
                BrowserStepEvaluation::DeadlineExceeded if cancellation.is_cancelled() => {
                    return Err(BrowserGraphError::Cancelled);
                }
                BrowserStepEvaluation::DeadlineExceeded => {
                    browser_context_fatal = true;
                    let partial = browser_timeout_result(&plan, &cursor, &step)
                        .map_err(BrowserSchedulerError::Protocol)?;
                    return serde_json::to_string(&partial).map_err(|encoding| {
                        BrowserSchedulerError::Protocol(format!(
                            "Wake scheduler could not encode a browser timeout result: {encoding}"
                        ))
                        .into()
                    });
                }
            }
            cursor = next_browser_scheduler_cursor(&page, timeout_ms)?;
        }
    })();
    let (result, infrastructure_error) = match graph_result {
        Ok(result) => (Ok(result), None),
        Err(BrowserGraphError::Cancelled) => {
            browser_context_fatal = true;
            (Err("Wake browser test was cancelled".to_string()), None)
        }
        Err(BrowserGraphError::Infrastructure(error)) => {
            browser_context_fatal = true;
            (Err(error.to_string()), Some(error))
        }
        Err(BrowserGraphError::Runtime(error)) => {
            browser_context_fatal = true;
            (Err(error), None)
        }
    };
    let reconcile_screenshots =
        result.is_ok() && !browser_context_fatal && !cancellation.is_cancelled();
    let operation_output = operation_dispatcher
        .finish(reconcile_screenshots)
        .map_err(|error| {
            let (code, message) = error
                .strip_prefix("WAKE_TEST_SNAPSHOT: ")
                .map_or(("WAKE_TEST_BROWSER", error.as_str()), |message| {
                    ("WAKE_TEST_SNAPSHOT", message)
                });
            TestError::new(code, message).at(path)
        })?;
    network_interception
        .finish()
        .map_err(|error| TestError::new("WAKE_TEST_NETWORK", error).at(path))?;
    let coverage =
        if coverage_enabled && !browser_context_fatal && !cancellation.is_cancelled() {
            Some(page.take_precise_coverage().map_err(|error| {
                TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(path)
            })?)
        } else {
            None
        };
    if let Some(error) = infrastructure_error {
        return Err(browser_infrastructure_error(error, path));
    }
    Ok((result, coverage, operation_output))
}

fn normalize_coverage(
    root: &Path,
    suite_path: &Path,
    compiled: &CompiledCommonJsGraphScript,
    raw: Option<&serde_json::Value>,
) -> Vec<NormalizedCoverageFile> {
    #[derive(Clone, Copy)]
    struct Range {
        start: usize,
        end: usize,
        count: u64,
    }

    fn covered_at(offset: usize, ranges: &[Range]) -> bool {
        ranges
            .iter()
            .filter(|range| range.start <= offset && offset < range.end)
            .min_by_key(|range| range.end.saturating_sub(range.start))
            .is_some_and(|range| range.count > 0)
    }

    fn original_line_starts(source: &str) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
        );
        starts
    }

    fn original_line(source: &str, line_starts: &[usize], byte_offset: u32) -> Option<u32> {
        let byte_offset = byte_offset as usize;
        if byte_offset >= source.len() || !source.is_char_boundary(byte_offset) {
            return None;
        }
        u32::try_from(line_starts.partition_point(|line_start| *line_start <= byte_offset)).ok()
    }

    fn mapped_original_byte(
        module: &wake_js_runtime::CommonJsModuleScriptLayout,
        absolute_utf16_offset: usize,
    ) -> Option<u32> {
        let body = module.body();
        if absolute_utf16_offset < body.utf16_start || absolute_utf16_offset >= body.utf16_end {
            return None;
        }
        let local_offset = absolute_utf16_offset - body.utf16_start;
        module.original_byte_offset_for_body_utf16_offset(local_offset)
    }

    fn mapped_original_range(
        module: &wake_js_runtime::CommonJsModuleScriptLayout,
        range: Range,
    ) -> Option<OriginalCoverageRange> {
        if range.start >= range.end || range.end > module.body().utf16_end {
            return None;
        }
        let start_byte = mapped_original_byte(module, range.start)?;
        let end_byte = mapped_original_byte(module, range.end - 1)?;
        Some(OriginalCoverageRange {
            start_byte: start_byte.min(end_byte),
            end_byte: start_byte.max(end_byte),
        })
    }

    let Some(raw) = raw else {
        return Vec::new();
    };
    let Some(script) = raw
        .get("result")
        .and_then(serde_json::Value::as_array)
        .and_then(|scripts| {
            scripts.iter().find(|script| {
                script.get("url").and_then(serde_json::Value::as_str) == Some(compiled.source_url())
            })
        })
    else {
        return Vec::new();
    };
    let functions = script
        .get("functions")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let function_ranges = functions
        .iter()
        .filter_map(|function| {
            let ranges = function.get("ranges")?.as_array()?;
            let parsed = ranges
                .iter()
                .filter_map(|range| {
                    Some(Range {
                        start: usize::try_from(range.get("startOffset")?.as_u64()?).ok()?,
                        end: usize::try_from(range.get("endOffset")?.as_u64()?).ok()?,
                        count: range.get("count")?.as_u64()?,
                    })
                })
                .collect::<Vec<_>>();
            (!parsed.is_empty()).then_some(parsed)
        })
        .collect::<Vec<_>>();
    let all_ranges = function_ranges
        .iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();

    compiled
        .modules()
        .iter()
        .filter_map(|module| {
            if module.is_synthetic() {
                return None;
            }
            let source = module.source_path();
            if source == suite_path
                || source
                    .components()
                    .any(|component| component.as_os_str() == "node_modules")
            {
                return None;
            }
            let path = source.strip_prefix(root).ok()?;
            let body = module.body();
            let line_starts = original_line_starts(module.original_source());
            let mut lines = BTreeMap::<u32, bool>::new();
            let mut range_anchor_hits = BTreeMap::<u32, bool>::new();
            for mapping in module.source_mappings() {
                let absolute_offset = body
                    .utf16_start
                    .saturating_add(mapping.generated_utf16_offset());
                if absolute_offset >= body.utf16_end {
                    continue;
                }
                let Some(line) = original_line(
                    module.original_source(),
                    &line_starts,
                    mapping.original_byte_offset(),
                ) else {
                    continue;
                };
                let covered = covered_at(absolute_offset, &all_ranges);
                lines
                    .entry(line)
                    .and_modify(|line_covered| *line_covered |= covered)
                    .or_insert(covered);
                range_anchor_hits
                    .entry(mapping.original_byte_offset())
                    // One source position can be emitted more than once (for example an export
                    // binding). It is a conservative range anchor only when every generated copy
                    // is covered; explicit V8 ranges remain authoritative when present.
                    .and_modify(|anchor_covered| *anchor_covered &= covered)
                    .or_insert(covered);
            }
            let mut functions = BTreeMap::<OriginalCoverageRange, bool>::new();
            let mut blocks = BTreeMap::<OriginalCoverageRange, bool>::new();
            for ranges in &function_ranges {
                if let Some(range) = ranges.first().copied()
                    && let Some(identity) = mapped_original_range(module, range)
                {
                    functions
                        .entry(identity)
                        .and_modify(|covered| *covered |= range.count > 0)
                        .or_insert(range.count > 0);
                }
                for range in ranges.iter().skip(1).copied() {
                    if let Some(identity) = mapped_original_range(module, range) {
                        blocks
                            .entry(identity)
                            .and_modify(|covered| *covered |= range.count > 0)
                            .or_insert(range.count > 0);
                    }
                }
            }
            Some(NormalizedCoverageFile {
                path: normalize_path(path),
                source: module.original_source().to_string(),
                lines,
                functions,
                blocks,
                range_anchor_hits,
            })
        })
        .collect()
}

fn aggregate_coverage(files: Vec<NormalizedCoverageFile>) -> AggregatedCoverage {
    #[derive(Clone, Copy)]
    enum RangeKind {
        Function,
        Block,
    }

    fn metric(covered: usize, total: usize) -> CoverageMetric {
        CoverageMetric {
            covered,
            total,
            percent: if total == 0 {
                100.0
            } else {
                covered as f64 * 100.0 / total as f64
            },
        }
    }

    fn ranges(
        execution: &NormalizedCoverageFile,
        kind: RangeKind,
    ) -> &BTreeMap<OriginalCoverageRange, bool> {
        match kind {
            RangeKind::Function => &execution.functions,
            RangeKind::Block => &execution.blocks,
        }
    }

    fn range_hits(
        executions: &[NormalizedCoverageFile],
        kind: RangeKind,
    ) -> BTreeMap<OriginalCoverageRange, bool> {
        let identities = executions
            .iter()
            .flat_map(|execution| ranges(execution, kind).keys().copied())
            .collect::<BTreeSet<_>>();
        identities
            .into_iter()
            .map(|identity| {
                let covered = executions.iter().any(|execution| {
                    // An explicit range is authoritative, including `count == 0`. Only a
                    // missing range may inherit the effective source-anchor coverage because
                    // V8 elides nested ranges whose count matches their parent.
                    ranges(execution, kind)
                        .get(&identity)
                        .copied()
                        .unwrap_or_else(|| {
                            execution
                                .range_anchor_hits
                                .range(identity.start_byte..=identity.end_byte)
                                .any(|(_, covered)| *covered)
                        })
                });
                (identity, covered)
            })
            .collect()
    }

    let mut grouped = BTreeMap::<String, Vec<NormalizedCoverageFile>>::new();
    for file in files {
        grouped.entry(file.path.clone()).or_default().push(file);
    }
    let mut files = Vec::with_capacity(grouped.len());
    let mut report_files = Vec::with_capacity(grouped.len());
    for (path, executions) in grouped {
        let mut lines = BTreeMap::<u32, bool>::new();
        for execution in &executions {
            for (line, covered) in &execution.lines {
                lines
                    .entry(*line)
                    .and_modify(|was_covered| *was_covered |= *covered)
                    .or_insert(*covered);
            }
        }
        let functions = range_hits(&executions, RangeKind::Function);
        let blocks = range_hits(&executions, RangeKind::Block);
        let source = executions
            .first()
            .map(|execution| execution.source.clone())
            .unwrap_or_default();
        debug_assert!(
            executions
                .iter()
                .all(|execution| execution.source == source)
        );
        files.push(CoverageFile {
            path: path.clone(),
            metrics: CoverageMetrics {
                lines: metric(
                    lines.values().filter(|covered| **covered).count(),
                    lines.len(),
                ),
                functions: metric(
                    functions.values().filter(|covered| **covered).count(),
                    functions.len(),
                ),
                blocks: metric(
                    blocks.values().filter(|covered| **covered).count(),
                    blocks.len(),
                ),
            },
        });
        report_files.push(CoverageReportFile {
            path,
            source,
            lines,
            functions,
            blocks,
        });
    }
    let mut summary = CoverageMetrics::default();
    for file in &files {
        summary.lines.covered += file.metrics.lines.covered;
        summary.lines.total += file.metrics.lines.total;
        summary.functions.covered += file.metrics.functions.covered;
        summary.functions.total += file.metrics.functions.total;
        summary.blocks.covered += file.metrics.blocks.covered;
        summary.blocks.total += file.metrics.blocks.total;
    }
    for metric in [
        &mut summary.lines,
        &mut summary.functions,
        &mut summary.blocks,
    ] {
        metric.percent = if metric.total == 0 {
            100.0
        } else {
            metric.covered as f64 * 100.0 / metric.total as f64
        };
    }
    AggregatedCoverage {
        result: CoverageResult {
            summary,
            files,
            report_artifact_ids: Vec::new(),
        },
        report_files,
    }
}

fn write_coverage_reports(
    root: &Path,
    reporters: &[wake_config::TestCoverageReporter],
    coverage: &CoverageResult,
    report_files: &[CoverageReportFile],
) -> Result<Vec<TestArtifact>, TestError> {
    let directory = root.join("coverage");
    let mut artifacts = Vec::new();
    for reporter in reporters {
        let (kind, path, contents) = match reporter {
            wake_config::TestCoverageReporter::Text => (
                "coverage-text",
                directory.join("coverage.txt"),
                coverage_text_report(coverage).into_bytes(),
            ),
            wake_config::TestCoverageReporter::Json => (
                "coverage-json",
                directory.join("wake-coverage.json"),
                serde_json::to_vec_pretty(coverage).expect("coverage is serializable"),
            ),
            wake_config::TestCoverageReporter::Lcov => (
                "coverage-lcov",
                directory.join("lcov.info"),
                coverage_lcov_report(report_files).into_bytes(),
            ),
            wake_config::TestCoverageReporter::Html => (
                "coverage-html",
                directory.join("index.html"),
                coverage_html_report(coverage, report_files).into_bytes(),
            ),
        };
        atomic_report_write(&path, &contents)?;
        let id = stable_id("artifact", &normalize_path(&path));
        artifacts.push(TestArtifact {
            id,
            kind: kind.to_string(),
            path: normalize_path(&path),
            suite_id: None,
            test_id: None,
            metadata: BTreeMap::new(),
        });
    }
    Ok(artifacts)
}

fn coverage_text_report(coverage: &CoverageResult) -> String {
    fn metric(metric: &CoverageMetric) -> String {
        format!(
            "{:.2}% ({}/{})",
            metric.percent, metric.covered, metric.total
        )
    }

    let mut output = String::from("Wake coverage\nFile\tLines\tFunctions\tBlocks\n");
    output.push_str(&format!(
        "All files\t{}\t{}\t{}\n",
        metric(&coverage.summary.lines),
        metric(&coverage.summary.functions),
        metric(&coverage.summary.blocks),
    ));
    for file in &coverage.files {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            file.path,
            metric(&file.metrics.lines),
            metric(&file.metrics.functions),
            metric(&file.metrics.blocks),
        ));
    }
    output
}

fn coverage_source_line(source: &str, byte_offset: u32) -> u32 {
    let offset = usize::try_from(byte_offset)
        .unwrap_or(usize::MAX)
        .min(source.len());
    u32::try_from(
        source.as_bytes()[..offset]
            .iter()
            .filter(|byte| **byte == b'\n')
            .count(),
    )
    .unwrap_or(u32::MAX)
    .saturating_add(1)
}

fn coverage_lcov_report(files: &[CoverageReportFile]) -> String {
    let mut output = String::new();
    for file in files {
        output.push_str("TN:Wake Test\n");
        output.push_str(&format!("SF:{}\n", file.path));
        for (index, (range, covered)) in file.functions.iter().enumerate() {
            let name = format!("wake_fn_{index}");
            let line = coverage_source_line(&file.source, range.start_byte);
            output.push_str(&format!("FN:{line},{name}\n"));
            output.push_str(&format!("FNDA:{},{name}\n", usize::from(*covered)));
        }
        output.push_str(&format!("FNF:{}\n", file.functions.len()));
        output.push_str(&format!(
            "FNH:{}\n",
            file.functions.values().filter(|covered| **covered).count()
        ));
        for (index, (range, covered)) in file.blocks.iter().enumerate() {
            let line = coverage_source_line(&file.source, range.start_byte);
            output.push_str(&format!(
                "BRDA:{line},{index},0,{}\n",
                usize::from(*covered)
            ));
        }
        output.push_str(&format!("BRF:{}\n", file.blocks.len()));
        output.push_str(&format!(
            "BRH:{}\n",
            file.blocks.values().filter(|covered| **covered).count()
        ));
        for (line, covered) in &file.lines {
            output.push_str(&format!("DA:{line},{}\n", usize::from(*covered)));
        }
        output.push_str(&format!("LF:{}\n", file.lines.len()));
        output.push_str(&format!(
            "LH:{}\n",
            file.lines.values().filter(|covered| **covered).count()
        ));
        output.push_str("end_of_record\n");
    }
    output
}

fn coverage_html_report(coverage: &CoverageResult, files: &[CoverageReportFile]) -> String {
    let summary_rows = coverage
        .files
        .iter()
        .map(|file| {
            format!(
                "<tr><td><a href=\"#{}\">{}</a></td><td>{:.2}%</td><td>{:.2}%</td><td>{:.2}%</td></tr>",
                stable_id("coverage-file", &file.path),
                html_escape(&file.path),
                file.metrics.lines.percent,
                file.metrics.functions.percent,
                file.metrics.blocks.percent,
            )
        })
        .collect::<String>();
    let source_sections = files
        .iter()
        .map(|file| {
            let source = file
                .source
                .split('\n')
                .enumerate()
                .map(|(index, source)| {
                    let line = u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1);
                    let class = match file.lines.get(&line) {
                        Some(true) => "covered",
                        Some(false) => "uncovered",
                        None => "neutral",
                    };
                    format!(
                        "<span class=\"line {class}\"><b>{line:>5}</b> {}</span>",
                        html_escape(source)
                    )
                })
                .collect::<String>();
            format!(
                "<section id=\"{}\"><h2>{}</h2><pre>{source}</pre></section>",
                stable_id("coverage-file", &file.path),
                html_escape(&file.path),
            )
        })
        .collect::<String>();
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Wake coverage</title><style>body{{font:14px system-ui;margin:2rem;color:#18212f}}table{{border-collapse:collapse}}th,td{{padding:.45rem .8rem;border:1px solid #ccd2da;text-align:right}}th:first-child,td:first-child{{text-align:left}}pre{{overflow:auto;border:1px solid #ccd2da;background:#f7f8fa}}.line{{display:block;padding:0 .6rem}}.line b{{display:inline-block;color:#667085;font-weight:400}}.covered{{background:#e9f8ee}}.uncovered{{background:#ffecec}}.neutral{{background:#f7f8fa}}</style></head><body><h1>Wake coverage</h1><table><thead><tr><th>File</th><th>Lines</th><th>Functions</th><th>Blocks</th></tr></thead><tbody>{summary_rows}</tbody></table>{source_sections}</body></html>"
    )
}

fn atomic_report_write(path: &Path, contents: &[u8]) -> Result<(), TestError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        TestError::new(
            "WAKE_TEST_COVERAGE",
            format!("could not create coverage directory: {error}"),
        )
        .at(parent)
    })?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".wake-coverage-")
        .tempfile_in(parent)
        .map_err(|error| TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(path))?;
    temporary
        .write_all(contents)
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(path))?;
    temporary
        .persist(path)
        .map_err(|error| TestError::new("WAKE_TEST_COVERAGE", error.error.to_string()).at(path))?;
    Ok(())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn suite_wrapper_source(
    root: &Path,
    suite_path: &Path,
    config: &wake_config::Test,
    source: &str,
) -> String {
    let mut output = String::new();
    if config.environment != wake_config::TestEnvironment::Browser {
        output.push_str("import '@wake-internal/happy-dom';\n");
    }
    if source.contains("@crab-dev/wake/test/react") {
        output.push_str("import 'react';\nimport 'react-dom/client';\n");
    }
    for setup in &config.setup {
        let setup = setup.replace("<rootDir>", &root.to_string_lossy());
        let setup = PathBuf::from(setup);
        let setup = if setup.is_absolute() {
            setup
        } else {
            root.join(setup)
        };
        let specifier = normalize_path(&setup);
        output.push_str("import ");
        output.push_str(&serde_json::to_string(&specifier).expect("setup path is a string"));
        output.push_str(";\n");
    }
    output.push_str("import ");
    output.push_str(
        &serde_json::to_string(&normalize_path(suite_path)).expect("suite path is a string"),
    );
    output.push_str(";\n");
    output
}

fn compile_optional_pattern(pattern: Option<&str>) -> Result<Option<Regex>, TestError> {
    pattern
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| {
                TestError::new(
                    "WAKE_TEST_CONFIG",
                    format!("invalid test name pattern {pattern:?}: {error}"),
                )
            })
        })
        .transpose()
}

fn parse_shard(shard: &str) -> Result<(usize, usize), TestError> {
    let Some((index, count)) = shard.split_once('/') else {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            "shard must use the form <index>/<count>",
        ));
    };
    let index = index.parse::<usize>().map_err(|_| {
        TestError::new("WAKE_TEST_CONFIG", "shard index must be a positive integer")
    })?;
    let count = count.parse::<usize>().map_err(|_| {
        TestError::new("WAKE_TEST_CONFIG", "shard count must be a positive integer")
    })?;
    if index == 0 || count == 0 || index > count {
        return Err(TestError::new(
            "WAKE_TEST_CONFIG",
            "shard index must be between 1 and shard count",
        ));
    }
    Ok((index, count))
}

fn seeded_path_key(path: &Path, seed: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    for byte in normalize_path(path).bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn discover_tests(
    root: &Path,
    config: &wake_config::Test,
    patterns: &[String],
    selected_projects: &[String],
    environment_override: Option<&str>,
) -> Result<Vec<DiscoveredTest>, TestError> {
    let patterns = patterns
        .iter()
        .map(|pattern| {
            Regex::new(pattern).map_err(|error| {
                TestError::new(
                    "WAKE_TEST_CONFIG",
                    format!("invalid test path pattern {pattern:?}: {error}"),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut discovered = Vec::new();
    let projects = if config.projects.is_empty() {
        vec![(None, root.to_path_buf(), config.clone())]
    } else {
        let projects = config
            .projects
            .iter()
            .filter(|project| {
                selected_projects.is_empty() || selected_projects.contains(&project.name)
            })
            .map(|project| project_config(root, config, project))
            .collect::<Vec<_>>();
        for requested in selected_projects {
            if !config
                .projects
                .iter()
                .any(|project| &project.name == requested)
            {
                return Err(TestError::new(
                    "WAKE_TEST_CONFIG",
                    format!("unknown test project {requested:?}"),
                ));
            }
        }
        projects
    };
    for (project_name, project_root, project_config) in projects {
        let file_matcher = TestFileMatcher::compile(&project_root, &project_config)?;
        let mut paths = Vec::new();
        collect_test_files(
            &project_root,
            &project_root,
            &patterns,
            &file_matcher,
            &mut paths,
        )?;
        paths.sort();
        paths.dedup();
        for path in paths {
            let environment = match environment_override {
                Some("dom") => wake_config::TestEnvironment::Dom,
                Some("browser") => wake_config::TestEnvironment::Browser,
                Some("auto") | None => match project_config.environment {
                    wake_config::TestEnvironment::Auto => {
                        file_matcher.environment(&path, &project_root)
                    }
                    environment => environment,
                },
                Some(_) => unreachable!("environment override is validated before discovery"),
            };
            let mut test_config = project_config.clone();
            test_config.environment = environment;
            discovered.push(DiscoveredTest {
                path,
                config: test_config,
                project: project_name.clone(),
                environment,
            });
        }
    }
    discovered.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(discovered)
}

#[derive(Clone)]
struct DiscoveredTest {
    path: PathBuf,
    config: wake_config::Test,
    project: Option<String>,
    environment: wake_config::TestEnvironment,
}

impl DiscoveredTest {
    fn identity(&self, root: &Path) -> SuiteIdentity {
        SuiteIdentity::new(root, &self.path, self.project.as_deref())
    }
}

#[derive(Clone)]
struct PreparedTest {
    discovered: DiscoveredTest,
    original_source: String,
    entry_path: PathBuf,
    graph: Result<Arc<CompiledCommonJsModuleGraph>, PreparedGraphError>,
}

#[derive(Debug, Clone)]
struct PreparedGraphError {
    code: &'static str,
    message: String,
    path: PathBuf,
    recovery_watch_paths: Vec<PathBuf>,
}

impl PreparedTest {
    fn graph(&self) -> Result<&CompiledCommonJsModuleGraph, TestError> {
        self.graph
            .as_deref()
            .map_err(|error| TestError::new(error.code, error.message.clone()).at(&error.path))
    }
}

fn prepare_test(root: &Path, discovered: DiscoveredTest) -> PreparedTest {
    let path = discovered.path.clone();
    let identity_label =
        suite_identity_label(&normalize_path(&path), discovered.project.as_deref());
    let entry_path = path.parent().unwrap_or(root).join(format!(
        ".wake-test-entry-{}.ts",
        stable_id("suite", &identity_label)
    ));
    let original_source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            return PreparedTest {
                discovered,
                original_source: String::new(),
                entry_path,
                graph: Err(PreparedGraphError {
                    code: "WAKE_TEST_RUNTIME",
                    message: format!("could not read test suite: {error}"),
                    recovery_watch_paths: vec![path.clone()],
                    path,
                }),
            };
        }
    };
    let source = suite_wrapper_source(root, &path, &discovered.config, &original_source);
    let graph = compile_commonjs_module_graph(&entry_path, &source)
        .map(Arc::new)
        .map_err(|error| {
            let recovery_watch_paths = error.recovery_watch_paths().to_vec();
            PreparedGraphError {
                code: "WAKE_TEST_RUNTIME",
                message: error.to_string(),
                path,
                recovery_watch_paths,
            }
        });
    PreparedTest {
        discovered,
        original_source,
        entry_path,
        graph,
    }
}

struct TestFileMatcher {
    patterns: Vec<Regex>,
    browser_patterns: Vec<Regex>,
    exclude_patterns: Vec<Regex>,
}

impl TestFileMatcher {
    fn compile(root: &Path, config: &wake_config::Test) -> Result<Self, TestError> {
        let compile = |values: &[String]| {
            values
                .iter()
                .map(|pattern| {
                    wake_glob_regex(&pattern.replace("<rootDir>", &normalize_path(root)))
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .map(|pattern| {
                    Regex::new(&pattern).map_err(|error| {
                        TestError::new(
                            "WAKE_TEST_CONFIG",
                            format!("invalid test discovery pattern {pattern:?}: {error}"),
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(Self {
            patterns: compile(&config.include)?,
            browser_patterns: compile(&config.browser_include)?,
            exclude_patterns: compile(&config.exclude)?,
        })
    }

    fn is_match(&self, path: &Path, root: &Path) -> bool {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let normalized = normalize_path(relative);
        let absolute = normalize_path(path);
        self.patterns
            .iter()
            .any(|pattern| pattern.is_match(&normalized) || pattern.is_match(&absolute))
    }

    fn environment(&self, path: &Path, root: &Path) -> wake_config::TestEnvironment {
        let relative = normalize_path(path.strip_prefix(root).unwrap_or(path));
        let absolute = normalize_path(path);
        if self
            .browser_patterns
            .iter()
            .any(|pattern| pattern.is_match(&relative) || pattern.is_match(&absolute))
        {
            wake_config::TestEnvironment::Browser
        } else {
            wake_config::TestEnvironment::Dom
        }
    }

    fn is_excluded(&self, path: &Path, root: &Path) -> bool {
        let relative = normalize_path(path.strip_prefix(root).unwrap_or(path));
        let absolute = normalize_path(path);
        let relative_directory = format!("{relative}/");
        let absolute_directory = format!("{absolute}/");
        self.exclude_patterns.iter().any(|pattern| {
            pattern.is_match(&relative)
                || pattern.is_match(&absolute)
                || pattern.is_match(&relative_directory)
                || pattern.is_match(&absolute_directory)
        })
    }
}

fn wake_glob_regex(pattern: &str) -> Result<String, TestError> {
    fn translate(
        pattern: &str,
        chars: &[char],
        index: &mut usize,
        nested: bool,
    ) -> Result<String, TestError> {
        let mut output = String::new();
        while *index < chars.len() {
            let character = chars[*index];
            if nested && character == ')' {
                *index += 1;
                return Ok(output);
            }
            if matches!(character, '?' | '+' | '*' | '@' | '!')
                && chars.get(*index + 1) == Some(&'(')
            {
                *index += 2;
                let inner = translate(pattern, chars, index, true)?;
                let suffix = match character {
                    '?' => "?",
                    '+' => "+",
                    '*' => "*",
                    '@' => "",
                    '!' => "",
                    _ => unreachable!(),
                };
                output.push_str("(?:");
                output.push_str(&inner);
                output.push(')');
                output.push_str(suffix);
                continue;
            }
            match character {
                '*' if chars.get(*index + 1) == Some(&'*') => {
                    *index += 2;
                    if chars.get(*index) == Some(&'/') {
                        *index += 1;
                        output.push_str("(?:.*/)?");
                    } else {
                        output.push_str(".*");
                    }
                    continue;
                }
                '*' => output.push_str("[^/]*"),
                '?' => output.push_str("[^/]"),
                '[' => {
                    let start = *index;
                    *index += 1;
                    while *index < chars.len() && chars[*index] != ']' {
                        *index += 1;
                    }
                    if *index == chars.len() {
                        return Err(TestError::new(
                            "WAKE_TEST_CONFIG",
                            format!("unclosed character class in test include pattern {pattern:?}"),
                        ));
                    }
                    output.extend(chars[start..=*index].iter());
                }
                '{' => output.push_str("(?:"),
                '}' => output.push(')'),
                ',' => output.push('|'),
                '(' => output.push_str("(?:"),
                ')' => output.push(')'),
                '|' | '/' => output.push(character),
                '\\' => output.push('/'),
                _ => {
                    if ".^$+{}[]".contains(character) {
                        output.push('\\');
                    }
                    output.push(character);
                }
            }
            *index += 1;
        }
        if nested {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                format!("unclosed extglob in test include pattern {pattern:?}"),
            ));
        }
        Ok(output)
    }

    let characters = pattern.replace('\\', "/").chars().collect::<Vec<_>>();
    let mut index = 0;
    translate(pattern, &characters, &mut index, false).map(|expression| format!("^{expression}$"))
}

fn project_config(
    root: &Path,
    parent: &wake_config::Test,
    project: &wake_config::TestProject,
) -> (Option<String>, PathBuf, wake_config::Test) {
    let configured_root = project.root.replace("<rootDir>", &root.to_string_lossy());
    let configured_root = PathBuf::from(configured_root);
    let project_root = if configured_root.is_absolute() {
        configured_root
    } else {
        root.join(configured_root)
    };
    let mut config = parent.clone();
    config.projects.clear();
    config.environment = project.environment;
    (Some(project.name.clone()), project_root, config)
}

fn configured_test_watch_roots(
    root: &Path,
    config: &wake_config::Test,
    selected_projects: &[String],
) -> Result<BTreeSet<PathBuf>, TestError> {
    if config.projects.is_empty() {
        return Ok(BTreeSet::from([normalize_watch_input(root, root)]));
    }
    for requested in selected_projects {
        if !config
            .projects
            .iter()
            .any(|project| &project.name == requested)
        {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                format!("unknown test project {requested:?}"),
            ));
        }
    }
    Ok(config
        .projects
        .iter()
        .filter(|project| selected_projects.is_empty() || selected_projects.contains(&project.name))
        .map(|project| project_config(root, config, project).1)
        .map(|path| normalize_watch_input(root, &path))
        .collect())
}

fn refresh_compiled_watch_paths(
    root: &Path,
    prepared: &[PreparedTest],
    watch_paths: &mut BTreeMap<PathBuf, WatchPathMetadata>,
) {
    for suite in prepared {
        let path = &suite.discovered.path;
        merge_watch_path(
            watch_paths,
            normalize_watch_input(root, path),
            false,
            TestWatchPathRoles::COMPILER_INPUT,
        );
        merge_watch_path(
            watch_paths,
            normalize_watch_input(
                root,
                &snapshot_path(
                    path,
                    &suite.discovered.config.snapshot.directory,
                    suite.discovered.project.as_deref(),
                ),
            ),
            false,
            TestWatchPathRoles::BASELINE_INPUT,
        );
        merge_watch_path(
            watch_paths,
            normalize_watch_input(
                root,
                &path
                    .parent()
                    .unwrap_or(root)
                    .join(&suite.discovered.config.snapshot.screenshot_directory),
            ),
            true,
            TestWatchPathRoles::BASELINE_INPUT,
        );
        match &suite.graph {
            Ok(graph) => {
                let manifest = graph.module_graph();
                for path in manifest
                    .modules
                    .iter()
                    .flat_map(|module| module.watch_paths.iter())
                    .chain(manifest.resolver_inputs.iter())
                {
                    merge_watch_path(
                        watch_paths,
                        normalize_watch_input(root, path),
                        false,
                        TestWatchPathRoles::COMPILER_INPUT,
                    );
                }
            }
            Err(error) => {
                for path in &error.recovery_watch_paths {
                    merge_watch_path(
                        watch_paths,
                        normalize_watch_input(root, path),
                        false,
                        TestWatchPathRoles::COMPILER_INPUT,
                    );
                }
            }
        }
    }
}

fn merge_watch_path(
    watch_paths: &mut BTreeMap<PathBuf, WatchPathMetadata>,
    path: PathBuf,
    recursive: bool,
    roles: TestWatchPathRoles,
) {
    watch_paths
        .entry(path)
        .and_modify(|metadata| {
            metadata.recursive |= recursive;
            metadata.roles = metadata.roles.union(roles);
        })
        .or_insert(WatchPathMetadata { recursive, roles });
}

fn normalize_watch_input(root: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    path.canonicalize().unwrap_or(path)
}

fn collect_test_files(
    directory: &Path,
    root: &Path,
    patterns: &[Regex],
    file_matcher: &TestFileMatcher,
    paths: &mut Vec<PathBuf>,
) -> Result<(), TestError> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TestError::new(
                "WAKE_TEST_DISCOVERY",
                format!("could not scan tests: {error}"),
            )
            .at(directory));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            TestError::new(
                "WAKE_TEST_DISCOVERY",
                format!("could not read test directory entry: {error}"),
            )
            .at(directory)
        })?;
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(&path);
        if file_matcher.is_excluded(&path, root) {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| {
            TestError::new(
                "WAKE_TEST_DISCOVERY",
                format!("could not inspect test path: {error}"),
            )
            .at(&path)
        })?;
        if file_type.is_dir() {
            if matches!(
                entry.file_name().to_str(),
                Some("node_modules" | ".git" | ".wake" | "target")
            ) {
                continue;
            }
            collect_test_files(&path, root, patterns, file_matcher, paths)?;
        } else if file_type.is_file()
            && is_supported_test_module(&path)
            && if patterns.is_empty() {
                file_matcher.is_match(&path, root)
            } else {
                patterns
                    .iter()
                    .any(|pattern| pattern.is_match(&normalize_path(relative)))
            }
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn is_supported_test_module(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx"
            )
        })
        .unwrap_or(false)
}

fn snapshot_path(test_path: &Path, directory: &str, project: Option<&str>) -> PathBuf {
    let file_name = test_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("test");
    let project_suffix = project
        .map(|project| format!(".{}", stable_id("project", project)))
        .unwrap_or_default();
    test_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(directory)
        .join(format!("{file_name}{project_suffix}.snap"))
}

fn load_snapshots(path: &Path) -> Result<BTreeMap<String, String>, TestError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => {
            return Err(TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("could not read snapshot file: {error}"),
            )
            .at(path));
        }
    };
    let mut snapshots = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let Some(assignment) = line.strip_prefix("exports[") else {
            continue;
        };
        let Some(assignment) = assignment.strip_suffix(';') else {
            return Err(TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("invalid snapshot assignment on line {}", index + 1),
            )
            .at(path));
        };
        let Some((key, value)) = assignment.rsplit_once("] = ") else {
            return Err(TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("invalid snapshot assignment on line {}", index + 1),
            )
            .at(path));
        };
        let key = serde_json::from_str::<String>(key).map_err(|error| {
            TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("invalid snapshot key on line {}: {error}", index + 1),
            )
            .at(path)
        })?;
        let value = serde_json::from_str::<String>(value).map_err(|error| {
            TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("invalid snapshot value on line {}: {error}", index + 1),
            )
            .at(path)
        })?;
        snapshots.insert(key, value);
    }
    Ok(snapshots)
}

fn reconcile_snapshots(
    path: &Path,
    mut expected: BTreeMap<String, String>,
    received: &[RuntimeSnapshot],
    update: SnapshotUpdate,
) -> Result<(SnapshotSummary, Vec<String>), TestError> {
    let mut summary = SnapshotSummary::default();
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    let mut changed = false;
    for snapshot in received {
        seen.insert(snapshot.key.clone());
        match expected.get(&snapshot.key) {
            Some(value) if value == &snapshot.value => summary.matched += 1,
            Some(_) if update == SnapshotUpdate::All => {
                expected.insert(snapshot.key.clone(), snapshot.value.clone());
                summary.updated += 1;
                changed = true;
            }
            Some(_) => summary.unmatched += 1,
            None if update != SnapshotUpdate::None => {
                expected.insert(snapshot.key.clone(), snapshot.value.clone());
                summary.added += 1;
                changed = true;
            }
            None => summary.unmatched += 1,
        }
    }
    let obsolete = expected
        .keys()
        .filter(|key| !seen.contains(*key))
        .cloned()
        .collect::<Vec<_>>();
    if update == SnapshotUpdate::All {
        for key in &obsolete {
            expected.remove(key);
        }
        if !obsolete.is_empty() {
            changed = true;
            summary.files_removed += usize::from(expected.is_empty());
        }
    } else {
        summary.obsolete = obsolete.len();
        failures.extend(
            obsolete
                .iter()
                .map(|key| format!("Obsolete snapshot: {key}")),
        );
    }
    if changed {
        write_snapshots(path, &expected)?;
    }
    Ok((summary, failures))
}

fn write_snapshots(path: &Path, snapshots: &BTreeMap<String, String>) -> Result<(), TestError> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| {
        TestError::new(
            "WAKE_TEST_SNAPSHOT",
            format!("could not create snapshot directory: {error}"),
        )
        .at(parent)
    })?;
    let mut output = String::from("// Wake Snapshot v1\n\n");
    for (key, value) in snapshots {
        let key = serde_json::to_string(key).expect("snapshot keys are serializable strings");
        let value = serde_json::to_string(value).expect("snapshot values are serializable strings");
        output.push_str(&format!("exports[{key}] = {value};\n"));
    }
    let mut temporary = tempfile::Builder::new()
        .prefix(".wake-snapshot-")
        .tempfile_in(parent)
        .map_err(|error| {
            TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("could not stage snapshot file: {error}"),
            )
            .at(path)
        })?;
    temporary
        .write_all(output.as_bytes())
        .and_then(|()| temporary.flush())
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| {
            TestError::new(
                "WAKE_TEST_SNAPSHOT",
                format!("could not write snapshot file: {error}"),
            )
            .at(path)
        })?;
    temporary.persist(path).map_err(|error| {
        TestError::new(
            "WAKE_TEST_SNAPSHOT",
            format!("could not replace snapshot file: {}", error.error),
        )
        .at(path)
    })?;
    Ok(())
}

fn failure(message: String) -> TestFailure {
    let code = message
        .split(|character: char| character == ':' || character.is_whitespace())
        .find(|value| value.starts_with("WAKE_TEST_"))
        .map(str::to_string);
    TestFailure {
        stack: Some(message.clone()),
        message,
        code,
        location: None,
        diff: None,
    }
}

fn structured_failure(
    failure: RuntimeFailure,
    source_mapper: &GraphSourceMapper<'_>,
) -> TestFailure {
    let location = failure
        .stack
        .as_deref()
        .and_then(|stack| source_mapper.map_stack(stack));
    TestFailure {
        message: failure.message,
        code: failure.code,
        stack: failure.stack,
        location,
        diff: failure.diff,
    }
}

fn validate_react_versions(importer: &Path) -> Result<ReactVersions, TestError> {
    fn manifest(importer: &Path, package: &str) -> Result<(PathBuf, String), TestError> {
        let mut directory = importer.parent();
        while let Some(current) = directory {
            let path = current
                .join("node_modules")
                .join(package)
                .join("package.json");
            if path.is_file() {
                let source = fs::read_to_string(&path).map_err(|error| {
                    TestError::new(
                        "WAKE_TEST_REACT_VERSION",
                        format!("could not read {package} package metadata: {error}"),
                    )
                    .at(&path)
                })?;
                let value =
                    serde_json::from_str::<serde_json::Value>(&source).map_err(|error| {
                        TestError::new(
                            "WAKE_TEST_REACT_VERSION",
                            format!("invalid {package} package metadata: {error}"),
                        )
                        .at(&path)
                    })?;
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        TestError::new(
                            "WAKE_TEST_REACT_VERSION",
                            format!("{package} package metadata has no version"),
                        )
                        .at(&path)
                    })?;
                return Ok((path, version.to_string()));
            }
            directory = current.parent();
        }
        Err(TestError::new(
            "WAKE_TEST_REACT_VERSION",
            format!(
                "{package} is required by @crab-dev/wake/test/react; install matching react and react-dom >=19.2.8 <19.3.0"
            ),
        )
        .at(importer))
    }

    fn supported(version: &str) -> bool {
        let core = version.split_once('-').map_or(version, |(core, _)| core);
        let mut parts = core.split('.').map(str::parse::<u64>);
        matches!(
            (parts.next(), parts.next(), parts.next(), parts.next()),
            (Some(Ok(19)), Some(Ok(2)), Some(Ok(patch)), None) if patch >= 8
        )
    }

    let (react_manifest, react) = manifest(importer, "react")?;
    let (react_dom_manifest, react_dom) = manifest(importer, "react-dom")?;
    let react_project = react_manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let react_dom_project = react_dom_manifest
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    if react != react_dom || react_project != react_dom_project || !supported(&react) {
        return Err(TestError::new(
            "WAKE_TEST_REACT_VERSION",
            format!(
                "Wake Test requires one matching react/react-dom version >=19.2.8 <19.3.0; resolved react {react:?} at {} and react-dom {react_dom:?} at {}",
                react_manifest.display(),
                react_dom_manifest.display()
            ),
        )
        .at(importer));
    }
    Ok(ReactVersions { react, react_dom })
}

fn environment_info(
    kind: &str,
    react_versions: Option<&ReactVersions>,
    browser: Option<&BrowserDriver>,
) -> TestEnvironmentInfo {
    TestEnvironmentInfo {
        kind: kind.to_string(),
        react: react_versions.map(|versions| versions.react.clone()),
        react_dom: react_versions.map(|versions| versions.react_dom.clone()),
        v8: JsRuntime::engine_version().to_string(),
        browser: browser.map(|driver| BrowserEnvironmentInfo {
            name: match driver.installation.kind {
                BrowserKind::Chrome => "chrome",
                BrowserKind::Edge => "edge",
                BrowserKind::Chromium => "chromium",
                BrowserKind::Unknown => "unknown",
            }
            .to_string(),
            version: driver.installation.version.clone(),
            headless: driver.is_headless(),
        }),
    }
}

fn stable_id(kind: &str, value: &str) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{kind}-{hash:016x}")
}

fn suite_identity_label(path: &str, project: Option<&str>) -> String {
    match project {
        Some(project) => format!("project:{project}:{path}"),
        None => format!("root:{path}"),
    }
}

fn merge_snapshot_summary(target: &mut SnapshotSummary, source: &SnapshotSummary) {
    target.added += source.added;
    target.matched += source.matched;
    target.unmatched += source.unmatched;
    target.updated += source.updated;
    target.obsolete += source.obsolete;
    target.files_removed += source.files_removed;
}

fn random_seed() -> u64 {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    timestamp ^ u64::from(std::process::id())
}

fn duration_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(source: &str) -> tempfile::TempDir {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(fixture.path().join("math.test.ts"), source).unwrap();
        fixture
    }

    fn options(root: &Path) -> TestOptions {
        TestOptions {
            root: Some(root.to_path_buf()),
            seed: Some("wake-seed".to_string()),
            serial: true,
            ..TestOptions::default()
        }
    }

    fn stored_zip(entries: &[(&str, &str)]) -> Vec<u8> {
        struct Record {
            name: String,
            size: u32,
            local_offset: u32,
        }

        let mut archive = Vec::new();
        let mut records = Vec::new();
        for (name, contents) in entries {
            let name = name.as_bytes();
            let contents = contents.as_bytes();
            let local_offset = u32::try_from(archive.len()).unwrap();
            archive.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&0_u16.to_le_bytes());
            archive.extend_from_slice(&[0; 4]);
            archive.extend_from_slice(&0_u32.to_le_bytes());
            archive.extend_from_slice(&u32::try_from(contents.len()).unwrap().to_le_bytes());
            archive.extend_from_slice(&u32::try_from(contents.len()).unwrap().to_le_bytes());
            archive.extend_from_slice(&u16::try_from(name.len()).unwrap().to_le_bytes());
            archive.extend_from_slice(&0_u16.to_le_bytes());
            archive.extend_from_slice(name);
            archive.extend_from_slice(contents);
            records.push(Record {
                name: String::from_utf8(name.to_vec()).unwrap(),
                size: u32::try_from(contents.len()).unwrap(),
                local_offset,
            });
        }

        let central_start = u32::try_from(archive.len()).unwrap();
        for record in &records {
            archive.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&0_u16.to_le_bytes());
            archive.extend_from_slice(&[0; 4]);
            archive.extend_from_slice(&0_u32.to_le_bytes());
            archive.extend_from_slice(&record.size.to_le_bytes());
            archive.extend_from_slice(&record.size.to_le_bytes());
            archive.extend_from_slice(&u16::try_from(record.name.len()).unwrap().to_le_bytes());
            archive.extend_from_slice(&0_u16.to_le_bytes());
            archive.extend_from_slice(&0_u16.to_le_bytes());
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&[0; 2]);
            archive.extend_from_slice(&[0; 4]);
            archive.extend_from_slice(&record.local_offset.to_le_bytes());
            archive.extend_from_slice(record.name.as_bytes());
        }
        let central_size = u32::try_from(archive.len()).unwrap() - central_start;
        archive.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
        archive.extend_from_slice(&[0; 2]);
        archive.extend_from_slice(&[0; 2]);
        let record_count = u16::try_from(records.len()).unwrap();
        archive.extend_from_slice(&record_count.to_le_bytes());
        archive.extend_from_slice(&record_count.to_le_bytes());
        archive.extend_from_slice(&central_size.to_le_bytes());
        archive.extend_from_slice(&central_start.to_le_bytes());
        archive.extend_from_slice(&0_u16.to_le_bytes());
        archive
    }

    #[test]
    fn wake_globs_match_test_and_spec_files() {
        let matcher = Regex::new(
            &wake_glob_regex("**/*.{test,spec}.{js,mjs,cjs,jsx,ts,mts,cts,tsx}")
                .expect("valid Wake glob"),
        )
        .unwrap();
        assert!(matcher.is_match("src/value.test.tsx"));
        assert!(matcher.is_match("value.spec.js"));
        assert!(matcher.is_match("src/value.test.mjs"));
        assert!(matcher.is_match("value.spec.cjs"));
        assert!(matcher.is_match("src/value.test.mts"));
        assert!(matcher.is_match("value.spec.cts"));
        assert!(!matcher.is_match("src/value.ts"));
    }

    #[test]
    fn discovery_supports_all_module_extensions_and_explicit_pattern_overrides() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(fixture.path().join("default.test.mjs"), "").unwrap();
        fs::write(fixture.path().join("view.browser.spec.cts"), "").unwrap();
        fs::write(fixture.path().join("selected.mjs"), "").unwrap();
        fs::write(fixture.path().join("selected.txt"), "").unwrap();
        fs::create_dir(fixture.path().join("dist")).unwrap();
        fs::write(fixture.path().join("dist/selected.mjs"), "").unwrap();

        let config = wake_config::Test::default();
        let browser_pattern = Regex::new(
            &wake_glob_regex(&config.browser_include[0]).expect("valid browser include"),
        )
        .unwrap();
        assert!(
            browser_pattern.is_match("view.browser.spec.cts"),
            "{}",
            browser_pattern.as_str()
        );
        let discovered = discover_tests(fixture.path(), &config, &[], &[], None).unwrap();
        assert_eq!(discovered.len(), 2);
        assert!(discovered.iter().any(|test| {
            test.path.ends_with("default.test.mjs")
                && test.environment == wake_config::TestEnvironment::Dom
        }));
        assert!(discovered.iter().any(|test| {
            test.path.ends_with("view.browser.spec.cts")
                && test.environment == wake_config::TestEnvironment::Browser
        }));

        let discovered = discover_tests(
            fixture.path(),
            &config,
            &[r"(?:^|/)selected(?:\.mjs|\.txt)$".to_string()],
            &[],
            None,
        )
        .unwrap();
        assert_eq!(discovered.len(), 1);
        assert!(discovered[0].path.ends_with("selected.mjs"));
    }

    #[test]
    fn related_selection_uses_direct_and_shared_compiled_dependencies() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::create_dir(fixture.path().join("src")).unwrap();
        let shared = fixture.path().join("src/shared.ts");
        let button = fixture.path().join("src/button.ts");
        fs::write(&shared, "export const shared = 1").unwrap();
        fs::write(&button, "export const button = 2").unwrap();
        fs::write(
            fixture.path().join("button.test.ts"),
            "import {test, expect} from '@crab-dev/wake/test'; import {button} from './src/button'; import {shared} from './src/shared'; test('button', () => expect(button + shared).toBe(3));",
        )
        .unwrap();
        fs::write(
            fixture.path().join("form.test.ts"),
            "import {test, expect} from '@crab-dev/wake/test'; import {shared} from './src/shared'; test('form', () => expect(shared).toBe(1));",
        )
        .unwrap();

        let mut direct = options(fixture.path());
        direct.related = vec![button];
        let direct = run_tests(direct).unwrap();
        assert!(direct.success, "{direct:#?}");
        assert_eq!(direct.suites.len(), 1);
        assert!(direct.suites[0].path.ends_with("button.test.ts"));

        let mut shared_options = options(fixture.path());
        shared_options.related = vec![shared];
        let shared_result = run_tests(shared_options).unwrap();
        assert!(shared_result.success, "{shared_result:#?}");
        assert_eq!(shared_result.suites.len(), 2);

        let mut unrelated = options(fixture.path());
        unrelated.related = vec![fixture.path().join("src/unrelated.ts")];
        let unrelated = run_tests(unrelated).unwrap();
        assert!(unrelated.success, "{unrelated:#?}");
        assert!(unrelated.suites.is_empty());
        assert!(unrelated.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "WAKE_TEST_DISCOVERY"
                && diagnostic.message.contains("No tests are related")
        }));
    }

    #[test]
    fn yarn_pnp_zip_virtual_exports_execute_and_share_physical_selection_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        fs::create_dir_all(root.join(".yarn/cache")).unwrap();
        fs::write(root.join("package.json"), r#"{"private":true}"#).unwrap();

        let react_archive = root.join(".yarn/cache/react.zip");
        fs::write(
            &react_archive,
            stored_zip(&[
                (
                    "node_modules/react/package.json",
                    r#"{"name":"react","version":"19.2.8","exports":{"./jsx-dev-runtime":"./jsx-dev-runtime.js"}}"#,
                ),
                (
                    "node_modules/react/jsx-dev-runtime.js",
                    "const element = (type, props) => ({type, props}); exports.jsxDEV = element; exports.jsx = element; exports.jsxs = element; exports.Fragment = Symbol.for('wake.fragment');",
                ),
            ]),
        )
        .unwrap();
        let api_archive = root.join(".yarn/cache/wake-pnp-api.zip");
        fs::write(
            &api_archive,
            stored_zip(&[
                (
                    "node_modules/wake-pnp-api/package.json",
                    r#"{"name":"wake-pnp-api","version":"1.0.0","exports":{".":{"import":"./index.mjs","default":"./index.cjs"},"./feature":"./feature.ts"}}"#,
                ),
                (
                    "node_modules/wake-pnp-api/index.mjs",
                    "import data from './data.json'; import legacy from './legacy.cjs'; export const answer = data.answer; export const legacyKind = legacy.kind;",
                ),
                (
                    "node_modules/wake-pnp-api/index.cjs",
                    "module.exports = {answer: -1, legacyKind: 'wrong-condition'};",
                ),
                (
                    "node_modules/wake-pnp-api/data.json",
                    r#"{"answer":42}"#,
                ),
                (
                    "node_modules/wake-pnp-api/legacy.cjs",
                    "module.exports = {kind: 'zip-cjs'};",
                ),
                (
                    "node_modules/wake-pnp-api/feature.ts",
                    "export const typed: number = 42;",
                ),
            ]),
        )
        .unwrap();

        let pnp_data = serde_json::json!({
            "enableTopLevelFallback": false,
            "fallbackExclusionList": [],
            "fallbackPool": [],
            "packageRegistryData": [
                [serde_json::Value::Null, [[serde_json::Value::Null, {
                    "packageLocation": "./",
                    "packageDependencies": [
                        ["react", "virtual:wake#npm:19.2.8"],
                        ["wake-pnp-api", "npm:1.0.0"]
                    ],
                    "linkType": "SOFT"
                }]]],
                ["react", [["virtual:wake#npm:19.2.8", {
                    "packageLocation": "./.yarn/__virtual__/react-virtual/0/cache/react.zip/node_modules/react/",
                    "packageDependencies": [["react", "virtual:wake#npm:19.2.8"]],
                    "linkType": "HARD"
                }]]],
                ["wake-pnp-api", [["npm:1.0.0", {
                    "packageLocation": "./.yarn/cache/wake-pnp-api.zip/node_modules/wake-pnp-api/",
                    "packageDependencies": [["wake-pnp-api", "npm:1.0.0"]],
                    "linkType": "HARD"
                }]]]
            ]
        });
        let pnp_data_path = root.join(".pnp.data.json");
        fs::write(
            &pnp_data_path,
            serde_json::to_vec_pretty(&pnp_data).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join(".pnp.cjs"),
            "throw new Error('Wake must load the data manifest without executing Yarn');",
        )
        .unwrap();
        fs::write(
            root.join("pnp-client.ts"),
            "import {answer} from 'wake-pnp-api'; export const loadAnswer = () => answer;",
        )
        .unwrap();

        let suite = root.join("pnp.test.tsx");
        fs::write(
            &suite,
            r#"import {expect, mock, test} from '@crab-dev/wake/test';
import {typed} from 'wake-pnp-api/feature';
const view = <section data-answer={typed}>PnP 🦀</section>;
test('executes the authoritative PnP graph', async () => {
  expect(view.type).toBe('section');
  expect(view.props['data-answer']).toBe(42);
  mock.module('wake-pnp-api', async () => { await Promise.resolve(); return {answer: 7}; });
  const mocked = await mock.import('wake-pnp-api');
  expect(mocked.answer).toBe(7);
  const client = await mock.import('./pnp-client');
  expect(client.loadAnswer()).toBe(7);
  const actual = await mock.actual('wake-pnp-api');
  expect(actual.answer).toBe(42);
  expect(actual.legacyKind).toBe('zip-cjs');
});"#,
        )
        .unwrap();

        let result = run_tests(options(&root)).unwrap();
        assert!(result.success, "{result:#?}");

        let config = wake_config::Test::default();
        let discovered = discover_tests(&root, &config, &[], &[], None).unwrap();
        let prepared = prepare_test(&root, discovered.into_iter().next().unwrap());
        let graph = prepared.graph().unwrap();
        let manifest = graph.module_graph();
        assert!(!manifest.opaque_dependencies, "{manifest:#?}");
        assert!(manifest.resolver_inputs.contains(&pnp_data_path));
        assert!(
            manifest
                .modules
                .iter()
                .any(|module| module.watch_paths.contains(&react_archive))
        );
        assert!(manifest.modules.iter().any(|module| {
            module
                .dependencies
                .iter()
                .any(|dependency| dependency.specifier == "react/jsx-dev-runtime")
        }));
        assert!(
            manifest
                .modules
                .iter()
                .any(|module| module.watch_paths.contains(&api_archive))
        );
        assert!(manifest.modules.iter().any(|module| {
            module.dependencies.iter().any(|dependency| {
                dependency.specifier == "wake-pnp-api"
                    && dependency.kind == wake_js_runtime::ModuleGraphDependencyKind::WakeRuntime
            })
        }));
        let emitted = emit_commonjs_graph_script(graph, &prepared.entry_path, "", "");
        assert!(emitted.modules().iter().any(|module| {
            normalize_path(module.source_path()).contains("/.yarn/__virtual__/")
                && module.source_path().ends_with("jsx-dev-runtime.js")
        }));

        let missing = compile_commonjs_module_graph(
            &root.join("missing-entry.ts"),
            "import 'wake-pnp-api/missing-export';",
        )
        .unwrap_err();
        assert!(missing.recovery_watch_paths().contains(&api_archive));
        assert!(missing.recovery_watch_paths().contains(&pnp_data_path));

        let mut index = SuiteGraphIndex::default();
        index.record(&root, &suite, None, manifest);
        let suite_identity = SuiteIdentity::new(&root, &suite, None);
        for origin in [RelatedOrigin::Explicit, RelatedOrigin::Changed] {
            let selected = index.select(&root, std::slice::from_ref(&react_archive), origin);
            assert_eq!(selected.suites, BTreeSet::from([suite_identity.clone()]));
            assert!(!selected.conservative, "{selected:#?}");
        }
        let structural = index.select(
            &root,
            std::slice::from_ref(&pnp_data_path),
            RelatedOrigin::Changed,
        );
        assert_eq!(structural.suites, BTreeSet::from([suite_identity]));
        assert!(structural.conservative);
    }

    #[test]
    fn synthetic_setup_entry_preserves_the_real_suite_source_and_watch_identity() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        let setup = root.join("setup.ts");
        let suite = root.join("view.test.tsx");
        fs::write(&setup, "globalThis.ready = true").unwrap();
        fs::create_dir_all(root.join("node_modules/react")).unwrap();
        fs::write(
            root.join("node_modules/react/package.json"),
            r#"{"exports":{"./jsx-dev-runtime":"./jsx-dev-runtime.js"}}"#,
        )
        .unwrap();
        fs::write(
            root.join("node_modules/react/jsx-dev-runtime.js"),
            "exports.jsxDEV = (type, props) => ({type, props});",
        )
        .unwrap();
        let source = "const lineOne = 1;\nconst lineTwo = <div />;\nexport {lineTwo};\n";
        fs::write(&suite, source).unwrap();
        let config = wake_config::Test {
            environment: wake_config::TestEnvironment::Browser,
            setup: vec![normalize_path(&setup)],
            ..wake_config::Test::default()
        };
        let prepared = prepare_test(
            &root,
            DiscoveredTest {
                path: suite.clone(),
                config,
                project: None,
                environment: wake_config::TestEnvironment::Browser,
            },
        );
        let emitted =
            emit_commonjs_graph_script(prepared.graph().unwrap(), &prepared.entry_path, "", "");
        let real = emitted
            .modules()
            .iter()
            .find(|module| module.source_path() == suite)
            .expect("real suite module");
        assert_eq!(real.original_source(), source);
        assert!(!real.is_synthetic());
        assert!(emitted.modules().iter().any(|module| module.is_synthetic()));
        assert!(
            emitted
                .module_graph()
                .modules
                .iter()
                .any(|module| { module.watch_paths.iter().any(|path| path == &suite) })
        );
        assert!(
            emitted
                .module_graph()
                .modules
                .iter()
                .any(|module| { module.watch_paths.iter().any(|path| path == &setup) })
        );
    }

    #[test]
    fn watch_session_recompiles_affected_graphs_and_reuses_unaffected_graphs() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        let a_source = fixture.path().join("a.ts");
        let b_source = fixture.path().join("b.ts");
        fs::write(&a_source, "export const value = 1").unwrap();
        fs::write(&b_source, "export const value = 1").unwrap();
        for name in ["a", "b"] {
            fs::write(
                fixture.path().join(format!("{name}.test.ts")),
                format!(
                    "import {{test, expect}} from '@crab-dev/wake/test'; import {{value}} from './{name}'; test('{name}', () => expect(value).toBe(1));"
                ),
            )
            .unwrap();
        }
        let run_options = options(fixture.path());
        let mut session = TestWorkspaceSession::new(run_options.clone());
        let first = session
            .run(run_options.clone(), TestCancellationToken::default())
            .unwrap();
        assert!(first.success, "{first:#?}");
        let a_suite = fixture.path().join("a.test.ts").canonicalize().unwrap();
        let b_suite = fixture.path().join("b.test.ts").canonicalize().unwrap();
        let a_identity = SuiteIdentity::new(fixture.path(), &a_suite, None);
        let b_identity = SuiteIdentity::new(fixture.path(), &b_suite, None);
        let a_before = Arc::as_ptr(session.cache.prepared[&a_identity].graph.as_ref().unwrap());
        let b_before = Arc::as_ptr(session.cache.prepared[&b_identity].graph.as_ref().unwrap());

        fs::write(&a_source, "export const value = 2").unwrap();
        let rerun = session
            .run_watch(
                run_options,
                TestWatchRequest {
                    invalidated_paths: vec![a_source],
                    selection: TestWatchSelection::Affected,
                    rediscover: false,
                },
                TestCancellationToken::default(),
            )
            .unwrap();
        assert!(
            !rerun.success,
            "the affected suite must observe the new module"
        );
        assert_eq!(rerun.suites.len(), 1);
        let a_after = Arc::as_ptr(session.cache.prepared[&a_identity].graph.as_ref().unwrap());
        let b_after = Arc::as_ptr(session.cache.prepared[&b_identity].graph.as_ref().unwrap());
        assert_ne!(a_before, a_after);
        assert_eq!(b_before, b_after);
    }

    #[test]
    fn warm_empty_and_manual_sessions_rediscover_test_topology() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        let mut run_options = options(fixture.path());
        run_options.allow_no_tests = true;
        let mut watch_session = TestWorkspaceSession::new(run_options.clone());
        let empty = watch_session
            .run(run_options.clone(), TestCancellationToken::default())
            .unwrap();
        assert_eq!(empty.counts.suites.total, 0);

        let suite = fixture.path().join("created.test.ts");
        fs::write(
            &suite,
            "import {test, expect} from '@crab-dev/wake/test'; test('created', () => expect(1).toBe(1));",
        )
        .unwrap();
        let watched = watch_session
            .run_watch(
                run_options.clone(),
                TestWatchRequest {
                    invalidated_paths: vec![suite.clone()],
                    selection: TestWatchSelection::Affected,
                    rediscover: false,
                },
                TestCancellationToken::default(),
            )
            .unwrap();
        assert_eq!(watched.counts.suites.total, 1);

        fs::remove_file(&suite).unwrap();
        let removed = watch_session
            .run(run_options.clone(), TestCancellationToken::default())
            .unwrap();
        assert_eq!(removed.counts.suites.total, 0);
        fs::write(
            &suite,
            "import {test} from '@crab-dev/wake/test'; test('manual', () => {});",
        )
        .unwrap();
        let manual = watch_session
            .run(run_options, TestCancellationToken::default())
            .unwrap();
        assert_eq!(manual.counts.suites.total, 1);
    }

    #[test]
    fn overlapping_projects_keep_distinct_cache_result_and_snapshot_identities() {
        let fixture = fixture(
            "import {test, expect} from '@crab-dev/wake/test'; test('same', () => expect({answer: 42}).toMatchSnapshot());",
        );
        fs::write(
            fixture.path().join("wake.config.toml"),
            r#"
[[test.projects]]
name = "alpha"
root = "."
environment = "dom"

[[test.projects]]
name = "beta"
root = "."
environment = "dom"
"#,
        )
        .unwrap();
        let mut run_options = options(fixture.path());
        run_options.serial = false;
        run_options.workers = Some(WorkerOverride::Count(2));
        let mut session = TestWorkspaceSession::new(run_options.clone());
        let result = session
            .run(run_options.clone(), TestCancellationToken::default())
            .unwrap();
        assert!(result.success, "{result:#?}");
        assert_eq!(result.suites.len(), 2);
        assert_eq!(session.cache.prepared.len(), 2);
        assert_ne!(result.suites[0].id, result.suites[1].id);
        assert_eq!(
            result
                .suites
                .iter()
                .filter_map(|suite| suite.project.as_deref())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["alpha", "beta"])
        );

        let suite = fixture.path().join("math.test.ts");
        let alpha_snapshot = snapshot_path(&suite, "__snapshots__", Some("alpha"));
        let beta_snapshot = snapshot_path(&suite, "__snapshots__", Some("beta"));
        assert_ne!(alpha_snapshot, beta_snapshot);
        assert!(alpha_snapshot.is_file(), "{}", alpha_snapshot.display());
        assert!(beta_snapshot.is_file(), "{}", beta_snapshot.display());
        assert!(
            alpha_snapshot
                .file_name()
                .is_some_and(|name| name.to_string_lossy().contains("project-"))
        );

        run_options.update_snapshots = Some("none".to_string());
        let warm = session
            .run(run_options, TestCancellationToken::default())
            .unwrap();
        assert!(warm.success, "{warm:#?}");
    }

    #[test]
    fn invalid_project_names_surface_as_test_configuration_errors() {
        let fixture = fixture("");
        fs::write(
            fixture.path().join("wake.config.toml"),
            concat!(
                "[[test.projects]]\nname = \"client\"\nroot = \".\"\n",
                "[[test.projects]]\nname = \"client\"\nroot = \"packages/client\"\n",
            ),
        )
        .unwrap();
        let error = run_tests(options(fixture.path())).unwrap_err();
        assert_eq!(error.code(), "WAKE_TEST_CONFIG");
        assert!(error.to_string().contains("must be unique"), "{error}");
    }

    #[test]
    fn workspace_watch_paths_union_project_tree_and_baseline_roles() {
        let fixture =
            fixture("import {test} from '@crab-dev/wake/test'; test('watch roles', () => {});");
        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test.snapshot]\nscreenshot_directory = \".\"\n",
        )
        .unwrap();
        let run_options = options(fixture.path());
        let mut session = TestWorkspaceSession::new(run_options.clone());
        let result = session
            .run(run_options, TestCancellationToken::default())
            .unwrap();
        assert!(result.success, "{result:#?}");

        let root = fixture.path().canonicalize().unwrap();
        let registration = session
            .watch_paths()
            .into_iter()
            .find(|registration| registration.path == root)
            .expect("project root registration");
        assert!(registration.recursive);
        assert!(
            registration
                .roles
                .contains(TestWatchPathRoles::PROJECT_TREE)
        );
        assert!(
            registration
                .roles
                .contains(TestWatchPathRoles::BASELINE_INPUT)
        );
    }

    #[test]
    fn workspace_watch_paths_include_external_modules_and_failure_witnesses() {
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path().join("app");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        let shared = workspace.path().join("shared.ts");
        fs::write(&shared, "export const value = 1;").unwrap();
        fs::write(
            root.join("external.test.ts"),
            "import {test, expect} from '@crab-dev/wake/test'; import {value} from '../shared'; test('external', () => expect(value).toBe(1));",
        )
        .unwrap();
        let run_options = options(&root);
        let mut session = TestWorkspaceSession::new(run_options.clone());
        session
            .run(run_options.clone(), TestCancellationToken::default())
            .unwrap();
        let shared = shared.canonicalize().unwrap();
        assert!(session.watch_paths().iter().any(|input| {
            input.path == shared && input.roles.contains(TestWatchPathRoles::COMPILER_INPUT)
        }));

        fs::write(
            root.join("external.test.ts"),
            "import '../missing-recovery';",
        )
        .unwrap();
        let result = session
            .run(run_options, TestCancellationToken::default())
            .unwrap();
        assert!(!result.success, "{result:#?}");
        assert_eq!(result.suites.len(), 1);
        assert_eq!(result.suites[0].status, TestStatus::Failed);
        assert_eq!(result.suites[0].failures.len(), 1);
        assert_eq!(
            result.suites[0].failures[0].code.as_deref(),
            Some("WAKE_TEST_RUNTIME")
        );
        assert!(session.watch_paths().iter().any(|input| {
            input.roles.contains(TestWatchPathRoles::COMPILER_INPUT)
                && normalize_path(&input.path).contains("missing-recovery")
        }));
    }

    #[test]
    fn private_runtime_result_v1_is_strict() {
        let value = serde_json::json!({
            "schemaVersion": RUNTIME_RESULT_SCHEMA,
            "status": "passed",
            "cases": [],
            "failures": [],
            "snapshots": [],
        });
        let result: RuntimeSuiteResult = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(result.schema_version, RUNTIME_RESULT_SCHEMA);

        let mut invalid = value;
        invalid["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<RuntimeSuiteResult>(invalid).is_err());
    }

    #[test]
    fn browser_scheduler_ack_distinguishes_deadlines_from_normal_completion() {
        let completed = serde_json::json!({
            "schemaVersion": RUNTIME_SCHEDULER_SCHEMA,
            "stepId": "step-1",
            "timedOut": false,
        });
        let timed_out = serde_json::json!({
            "schemaVersion": RUNTIME_SCHEDULER_SCHEMA,
            "stepId": "step-1",
            "timedOut": true,
        });

        assert!(!parse_scheduler_ack(&completed.to_string(), "step-1").unwrap());
        assert!(parse_scheduler_ack(&timed_out.to_string(), "step-1").unwrap());

        let mut ambiguous = completed;
        ambiguous.as_object_mut().unwrap().remove("timedOut");
        assert!(parse_scheduler_ack(&ambiguous.to_string(), "step-1").is_err());
    }

    #[test]
    fn browser_realm_enables_react_act_before_runtime_and_module_bootstrap() {
        let prelude = format!("{BROWSER_HOST_PRELUDE}\n{WAKE_TEST_RUNTIME}");
        let act_environment = prelude
            .find("globalThis.IS_REACT_ACT_ENVIRONMENT = true")
            .expect("browser realm act initialization");
        let runtime_bootstrap = prelude
            .find("const runtimeResultSchema")
            .expect("Wake test runtime bootstrap");

        assert!(act_environment < runtime_bootstrap);
        assert!(
            BROWSER_HOST_PRELUDE
                .trim_start()
                .starts_with("globalThis.IS_REACT_ACT_ENVIRONMENT = true")
        );
    }

    #[test]
    fn browser_infrastructure_errors_never_become_case_timeouts() {
        let error = browser_infrastructure_error(
            BrowserSchedulerError::Browser(BrowserError {
                code: "WAKE_TEST_BROWSER",
                message: "CDP dispatcher disconnected".to_string(),
                path: None,
            }),
            Path::new("math.browser.test.ts"),
        );

        assert_eq!(error.code(), "WAKE_TEST_BROWSER");
        assert!(error.to_string().contains("CDP dispatcher disconnected"));
        assert!(!error.to_string().contains("WAKE_TEST_TIMEOUT"));
    }

    #[test]
    fn stack_frames_parse_windows_file_urls_async_frames_and_unicode() {
        assert_eq!(
            parse_stack_frame(r"    at async load (C:\Wake Project\src\🦀.test.tsx:12:34)"),
            Some(StackFramePosition {
                source: r"C:\Wake Project\src\🦀.test.tsx".to_string(),
                line: 12,
                column: 34,
            })
        );
        assert_eq!(
            parse_stack_frame(
                "    at async file:///C:/Wake%20Project/src/%F0%9F%A6%80.test.tsx:12:34"
            ),
            Some(StackFramePosition {
                source: "file:///C:/Wake%20Project/src/%F0%9F%A6%80.test.tsx".to_string(),
                line: 12,
                column: 34,
            })
        );
        let windows = normalize_stack_source(r"//?/C:\Wake Project\src\🦀.test.tsx").unwrap();
        let file_url =
            normalize_stack_source("file:///C:/Wake%20Project/src/%F0%9F%A6%80.test.tsx").unwrap();
        assert!(same_stack_source(&windows, &file_url));

        let stack = "Error: boom\n    at dependency (file:///C:/Wake/dependency.js:9:2)\n    at async file:///C:/Wake/suite.test.ts:4:9";
        let frames = stack
            .lines()
            .filter_map(parse_stack_frame)
            .collect::<Vec<_>>();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].line, 4);
        assert_eq!(frames[1].column, 9);
        assert!(parse_stack_frame("Error: at C:/Wake/suite.test.ts:4:9").is_none());
        assert!(parse_stack_frame("    at C:/Wake/suite.test.ts:0:9").is_none());
        assert!(parse_stack_frame("    at C:/Wake/suite.test.ts:4:0").is_none());
        assert!(normalize_stack_source("file:///C:/Wake/%GG.ts").is_none());
    }

    #[test]
    fn generated_stack_columns_are_strict_utf16_coordinates() {
        let lines = generated_script_lines("a🦀b\r\nnext");
        assert_eq!(
            lines,
            vec![
                GeneratedScriptLine {
                    utf16_start: 0,
                    utf16_end: 4,
                },
                GeneratedScriptLine {
                    utf16_start: 6,
                    utf16_end: 10,
                },
            ]
        );
    }

    #[test]
    fn private_browser_operation_v1_is_strict_and_owned() {
        let payload = serde_json::json!({
            "schemaVersion": BROWSER_OPERATION_SCHEMA,
            "id": "7",
            "action": "upload",
            "target": {"x": 12.5, "y": 24.0},
            "selector": "[data-wake-browser-input=\"7\"]",
            "files": [{"name": "wake.txt", "bytes": [87, 97, 107, 101]}],
        });
        let command: BrowserOperationCommand = serde_json::from_value(payload.clone()).unwrap();
        assert_eq!(command.schema_version(), BROWSER_OPERATION_SCHEMA);
        assert_eq!(command.id(), "7");
        let BrowserOperationCommand::Upload { files, .. } = command else {
            panic!("upload payload decoded as a different command")
        };
        assert_eq!(files[0].name, "wake.txt");
        assert_eq!(files[0].bytes, b"Wake");

        let mut invalid = payload;
        invalid["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<BrowserOperationCommand>(invalid).is_err());

        let failure = Err(BrowserOperationFailure::browser("target is obscured"));
        let expression = browser_operation_completion_expression("7", &failure).unwrap();
        assert!(expression.contains("__wakeCompleteBrowserOperation"));
        assert!(expression.contains("wake.browser.operation.v1"));
        assert!(expression.contains("target is obscured"));
    }

    #[test]
    fn screenshot_profile_hash_includes_fixed_reduced_motion() {
        let options = BrowserLaunchOptions {
            color_scheme: "dark".to_string(),
            ..BrowserLaunchOptions::default()
        };
        let profile = browser_screenshot_profile(
            "windows",
            "x86_64",
            BrowserKind::Chrome,
            "Google Chrome 140.2.1",
            &options,
        );
        let mut profile_value: serde_json::Value = serde_json::from_str(&profile).unwrap();
        assert_eq!(profile_value["reducedMotion"], REDUCED_MOTION);
        assert_eq!(profile_value["browserMajor"], "140");

        let profiled_hash = stable_id("screenshot-profile", &profile);
        profile_value
            .as_object_mut()
            .unwrap()
            .remove("reducedMotion");
        let unprofiled_hash = stable_id("screenshot-profile", &profile_value.to_string());
        assert_ne!(profiled_hash, unprofiled_hash);
    }

    #[test]
    fn screenshot_baselines_are_profiled_atomic_and_emit_owned_diffs() {
        let fixture = tempfile::tempdir().unwrap();
        let directory = fixture.path().join("__screenshots__");
        let policy = |update| BrowserScreenshotPolicy {
            difference_directory: directory.join("__diffs__"),
            directory: directory.clone(),
            suite_id: "suite-wake".to_string(),
            suite_path: "view.browser.test.tsx".to_string(),
            baseline_prefix: "wake-v1--suite--profile--".to_string(),
            profile_hash: "profile-wake".to_string(),
            update,
            seen: BTreeSet::new(),
        };
        let key = "visual card 1";
        let test_name = "visual card";

        let mut created_policy = policy(SnapshotUpdate::New);
        let mut created = BrowserOperationOutput::default();
        let outcome = created_policy
            .compare_png(key, test_name, b"png-one", &mut created)
            .unwrap();
        assert_eq!(outcome["pass"], true);
        assert_eq!(created.snapshot.added, 1);
        assert_eq!(created.artifacts.len(), 1);
        assert_eq!(
            created.artifacts[0].metadata["reducedMotion"],
            REDUCED_MOTION
        );
        let baseline_name = created_policy.baseline_file_name(key);
        let baseline = directory.join(&baseline_name);
        assert_eq!(fs::read(&baseline).unwrap(), b"png-one");

        let mut matched_policy = policy(SnapshotUpdate::New);
        let mut matched = BrowserOperationOutput::default();
        assert_eq!(
            matched_policy
                .compare_png(key, test_name, b"png-one", &mut matched)
                .unwrap()["pass"],
            true
        );
        assert_eq!(matched.snapshot.matched, 1);

        let mut changed_policy = policy(SnapshotUpdate::New);
        let mut changed = BrowserOperationOutput::default();
        let outcome = changed_policy
            .compare_png(key, test_name, b"png-two", &mut changed)
            .unwrap();
        assert_eq!(outcome["pass"], false);
        assert_eq!(changed.snapshot.unmatched, 1);
        assert_eq!(
            changed
                .artifacts
                .iter()
                .map(|artifact| artifact.kind.as_str())
                .collect::<Vec<_>>(),
            [
                "screenshot-baseline",
                "screenshot-received",
                "screenshot-diff"
            ]
        );
        assert!(
            changed
                .artifacts
                .iter()
                .all(|artifact| artifact.metadata["reducedMotion"] == REDUCED_MOTION)
        );
        let diff = changed
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "screenshot-diff")
            .unwrap();
        let diff_html = fs::read_to_string(&diff.path).unwrap();
        assert!(diff_html.contains("data:image/png;base64"));
        assert!(diff_html.contains("Difference overlay"));

        let mut updated_policy = policy(SnapshotUpdate::All);
        let mut updated = BrowserOperationOutput::default();
        assert_eq!(
            updated_policy
                .compare_png(key, test_name, b"png-two", &mut updated)
                .unwrap()["pass"],
            true
        );
        assert_eq!(updated.snapshot.updated, 1);
        assert_eq!(fs::read(&baseline).unwrap(), b"png-two");
        assert!(
            !directory
                .join("__diffs__")
                .join(format!(
                    "{}.received.png",
                    baseline_name.trim_end_matches(".png")
                ))
                .exists()
        );

        let obsolete = directory.join(format!(
            "{}obsolete--snapshot-deadbeef.png",
            updated_policy.baseline_prefix
        ));
        fs::write(&obsolete, b"old").unwrap();

        let mut stale_policy = policy(SnapshotUpdate::New);
        let mut stale = BrowserOperationOutput::default();
        stale_policy
            .compare_png(key, test_name, b"png-two", &mut stale)
            .unwrap();
        stale_policy.finish(&mut stale).unwrap();
        assert_eq!(stale.snapshot.obsolete, 1);
        assert_eq!(stale.failures.len(), 1);
        assert!(obsolete.exists());

        let mut cleanup_policy = policy(SnapshotUpdate::All);
        let mut cleanup = BrowserOperationOutput::default();
        cleanup_policy
            .compare_png(key, test_name, b"png-two", &mut cleanup)
            .unwrap();
        cleanup_policy.finish(&mut cleanup).unwrap();
        assert_eq!(cleanup.snapshot.files_removed, 1);
        assert!(!obsolete.exists());
    }

    #[test]
    fn runs_named_wake_imports_hooks_async_tests_and_skips() {
        let fixture = fixture(
            r#"
                import {beforeEach, describe, expect, test} from '@crab-dev/wake/test'
                let value: number = 0
                beforeEach(() => { value += 1 })
                describe('math', () => {
                    test('adds', async () => {
                        await Promise.resolve()
                        expect(value + 1).toBe(2)
                    })
                    test.skip('later', () => {})
                })
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        assert_eq!(result.schema_version, "wake.test.v1");
        assert_eq!(result.counts.tests.total, 2);
        assert_eq!(result.counts.tests.passed, 1);
        assert_eq!(result.counts.tests.skipped, 1);
        assert_eq!(result.seed, "wake-seed");
    }

    #[test]
    fn assertion_failure_resolves_to_a_structured_result() {
        let fixture = fixture(concat!(
            "import {expect, test} from '@crab-dev/wake/test'\n",
            "test('fails', async () => {\n",
            "  await Promise.resolve()\n",
            "  const marker = '🦀'; expect(marker).toBe('other')\n",
            "})\n",
        ));
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success);
        assert_eq!(result.counts.tests.failed, 1);
        assert!(
            result.suites[0].tests[0].failures[0]
                .message
                .contains("Expected \"🦀\" to be \"other\"")
        );
        let failure = &result.suites[0].tests[0].failures[0];
        assert_eq!(failure.code.as_deref(), Some("WAKE_TEST_ASSERTION"));
        assert_eq!(
            failure
                .diff
                .as_ref()
                .and_then(|diff| diff.expected.as_deref()),
            Some("\"other\"")
        );
        assert_eq!(
            failure
                .diff
                .as_ref()
                .and_then(|diff| diff.received.as_deref()),
            Some("\"🦀\"")
        );
        assert!(
            failure
                .diff
                .as_ref()
                .and_then(|diff| diff.unified.as_deref())
                .is_some_and(|diff| diff.contains("--- Expected"))
        );
        assert!(
            failure
                .stack
                .as_deref()
                .is_some_and(|stack| stack.contains(".wake-test-entry-")),
            "{:?}",
            failure.stack
        );
        let failure_location = failure.location.as_ref().expect("failure source location");
        assert_eq!(failure_location.path, "math.test.ts");
        assert_eq!(failure_location.line, 4);
        assert_eq!(failure_location.column, 31, "{failure_location:#?}");
        let case_location = result.suites[0].tests[0]
            .location
            .as_ref()
            .expect("test registration source location");
        assert_eq!(case_location.path, "math.test.ts");
        assert_eq!(case_location.line, 2);
        assert_eq!(case_location.column, 1, "{case_location:#?}");
    }

    #[test]
    fn suite_compile_and_top_level_errors_resolve_without_aborting_other_suites() {
        let fixture = tempfile::tempdir().unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("syntax.test.ts"),
            "export const broken: = 1",
        )
        .unwrap();
        fs::write(
            fixture.path().join("missing.test.ts"),
            "import './missing-module';",
        )
        .unwrap();
        fs::write(
            fixture.path().join("top-level.test.ts"),
            "throw new Error('top-level boom')",
        )
        .unwrap();
        fs::write(
            fixture.path().join("passing.test.ts"),
            concat!(
                "import {expect, test} from '@crab-dev/wake/test'\n",
                "test('still executes', () => expect(6 * 7).toBe(42))\n",
            ),
        )
        .unwrap();

        for serial in [true, false] {
            let mut run_options = options(fixture.path());
            run_options.serial = serial;
            if !serial {
                run_options.workers = Some(WorkerOverride::Count(2));
            }
            let result = run_tests(run_options).expect("suite-owned failures resolve as a result");
            assert!(!result.success, "{result:#?}");
            assert_eq!(result.counts.suites.total, 4, "{result:#?}");
            assert_eq!(result.counts.suites.failed, 3, "{result:#?}");
            assert_eq!(result.counts.suites.passed, 1, "{result:#?}");
            assert_eq!(result.counts.tests.passed, 1, "{result:#?}");
            for path in ["syntax.test.ts", "missing.test.ts"] {
                let suite = result
                    .suites
                    .iter()
                    .find(|suite| suite.path == path)
                    .expect("compiled failing suite is present");
                assert_eq!(suite.status, TestStatus::Failed, "{suite:#?}");
                assert_eq!(
                    suite.failures[0].code.as_deref(),
                    Some("WAKE_TEST_RUNTIME"),
                    "{suite:#?}"
                );
            }
            let top_level = result
                .suites
                .iter()
                .find(|suite| suite.path == "top-level.test.ts")
                .expect("top-level failing suite is present");
            assert_eq!(top_level.status, TestStatus::Failed, "{top_level:#?}");
            assert!(
                top_level.failures[0].message.contains("top-level boom"),
                "{top_level:#?}"
            );
        }
    }

    #[test]
    fn watch_session_recovers_after_a_suite_compile_error() {
        let fixture = fixture("export const broken: = 1");
        let test_path = fixture.path().join("math.test.ts");
        let run_options = options(fixture.path());
        let mut session = TestWorkspaceSession::new(run_options.clone());
        let first = session
            .run_watch(
                run_options.clone(),
                TestWatchRequest {
                    invalidated_paths: Vec::new(),
                    selection: TestWatchSelection::All,
                    rediscover: false,
                },
                TestCancellationToken::default(),
            )
            .expect("compile failure resolves as a watch result");
        assert!(!first.success, "{first:#?}");
        assert_eq!(first.suites[0].status, TestStatus::Failed);
        let canonical_test_path = test_path.canonicalize().unwrap();
        assert!(session.watch_paths().iter().any(|entry| {
            entry.path == canonical_test_path
                && entry.roles.contains(TestWatchPathRoles::COMPILER_INPUT)
        }));

        fs::write(
            &test_path,
            concat!(
                "import {expect, test} from '@crab-dev/wake/test'\n",
                "test('fixed', () => expect(true).toBe(true))\n",
            ),
        )
        .unwrap();
        let recovered = session
            .run_watch(
                run_options,
                TestWatchRequest {
                    invalidated_paths: vec![test_path],
                    selection: TestWatchSelection::Affected,
                    rediscover: false,
                },
                TestCancellationToken::default(),
            )
            .expect("watch remains reusable after a compile failure");
        assert!(recovered.success, "{recovered:#?}");
        assert_eq!(recovered.counts.tests.passed, 1, "{recovered:#?}");
    }

    #[test]
    fn async_dependency_failure_prefers_the_suite_frame_over_node_modules() {
        let fixture = fixture(concat!(
            "import {test} from '@crab-dev/wake/test'\n",
            "import {explode} from './node_modules/wake-boom/index.ts'\n",
            "test('dependency failure', async () => {\n",
            "  await explode()\n",
            "})\n",
        ));
        let dependency = fixture.path().join("node_modules/wake-boom");
        fs::create_dir_all(&dependency).unwrap();
        fs::write(
            dependency.join("index.ts"),
            concat!(
                "export async function explode() {\n",
                "  await Promise.resolve()\n",
                "  throw new Error('dependency boom')\n",
                "}\n",
            ),
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success, "{result:#?}");
        let case = &result.suites[0].tests[0];
        let failure = &case.failures[0];
        assert!(
            failure
                .stack
                .as_deref()
                .is_some_and(|stack| stack.contains("explode")),
            "{:?}",
            failure.stack
        );
        assert_eq!(
            failure.location,
            Some(TestLocation {
                path: "math.test.ts".to_string(),
                line: 4,
                column: 3,
                end_line: None,
                end_column: None,
            })
        );
        assert_eq!(
            case.location,
            Some(TestLocation {
                path: "math.test.ts".to_string(),
                line: 3,
                column: 1,
                end_line: None,
                end_column: None,
            })
        );
    }

    #[test]
    fn setup_failure_maps_to_setup_source_without_synthetic_wrapper_leakage() {
        let fixture =
            fixture("import {test} from '@crab-dev/wake/test'\ntest('unreached', () => {})\n");
        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test]\nsetup = [\"<rootDir>/setup.ts\"]\n",
        )
        .unwrap();
        fs::write(
            fixture.path().join("setup.ts"),
            "throw new Error('setup boom')\n",
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success, "{result:#?}");
        let failure = &result.suites[0].failures[0];
        let location = failure.location.as_ref().expect("setup source location");
        assert_eq!(location.path, "setup.ts");
        assert_eq!(location.line, 1);
        assert!(!location.path.contains(".wake-test-entry-"));
        assert!(
            failure
                .stack
                .as_deref()
                .is_some_and(|stack| stack.contains(".wake-test-entry-"))
        );
    }

    #[test]
    fn fast_dom_uses_the_npm_locked_same_realm_window() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('DOM', () => {
                    const root = document.createElement('main')
                    root.innerHTML = '<button aria-label="Wake">Run</button>'
                    document.body.appendChild(root)
                    expect(globalThis === window && window === self).toBe(true)
                    expect(document.defaultView === globalThis).toBe(true)
                    expect(root.querySelector('[aria-label="Wake"]').textContent).toBe('Run')
                })
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn external_snapshots_use_the_wake_v1_header() {
        let fixture = fixture(
            "import {expect, test} from '@crab-dev/wake/test'; test('snapshot', () => expect({answer: 42}).toMatchSnapshot())",
        );
        let created = run_tests(options(fixture.path())).unwrap();
        assert!(created.success, "{created:#?}");
        assert_eq!(created.snapshot.added, 1);
        let snapshot = snapshot_path(&fixture.path().join("math.test.ts"), "__snapshots__", None);
        assert!(
            fs::read_to_string(snapshot)
                .unwrap()
                .starts_with("// Wake Snapshot v1")
        );
    }

    #[test]
    fn screenshot_matcher_rejects_the_fast_dom_with_a_stable_browser_code() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('browser evidence only', async () => {
                    let failure
                    try { await expect(document).toMatchScreenshot('dom') } catch (error) { failure = error }
                    expect(failure.code).toBe('WAKE_TEST_BROWSER')
                    expect(failure.message).toContain('requires the browser environment')
                })
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        assert!(result.artifacts.is_empty());
    }

    #[test]
    fn snapshots_canonicalize_dom_and_replace_property_matchers() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('canonical DOM', () => {
                    const element = document.createElement('div')
                    element.setAttribute('z-last', '2')
                    element.setAttribute('a-first', '1')
                    element.appendChild(document.createTextNode('Wake < React'))
                    expect({element, requestId: 'run-specific-value'}).toMatchSnapshot({
                        requestId: expect.any(String),
                    })
                })
            "#,
        );

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        let snapshot = fs::read_to_string(snapshot_path(
            &fixture.path().join("math.test.ts"),
            "__snapshots__",
            None,
        ))
        .unwrap();
        assert!(
            snapshot.contains("<div a-first=\\\"1\\\" z-last=\\\"2\\\">Wake &lt; React</div>"),
            "{snapshot}"
        );
        assert!(snapshot.contains("Any<[Function String]>"), "{snapshot}");
        assert!(!snapshot.contains("run-specific-value"), "{snapshot}");
    }

    #[test]
    fn custom_matchers_equality_testers_and_snapshot_serializers_are_executed() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'

                expect.extend({
                    toBeEven(received) {
                        return {pass: typeof received === 'number' && received % 2 === 0, message: () => 'Expected an even number'}
                    },
                })
                expect.addEqualityTesters([
                    (left, right) => left?.kind === 'token' && right?.kind === 'token'
                        ? left.value.toLowerCase() === right.value.toLowerCase()
                        : undefined,
                ])
                expect.addSnapshotSerializer({
                    test: value => value?.kind === 'token',
                    print: value => `Token<${value.value}>`,
                })

                test('extensions', () => {
                    expect(4).toBeEven()
                    expect({kind: 'token', value: 'Wake'}).toEqual({kind: 'token', value: 'WAKE'})
                    expect({kind: 'token', value: 'serialized'}).toMatchSnapshot()
                })
            "#,
        );

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        let snapshot = fs::read_to_string(snapshot_path(
            &fixture.path().join("math.test.ts"),
            "__snapshots__",
            None,
        ))
        .unwrap();
        assert!(snapshot.contains("Token<serialized>"), "{snapshot}");
    }

    #[test]
    fn test_entry_exports_only_wake_owned_apis() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-test-entry-exports-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("math.test.ts"),
            "import * as wake from '@crab-dev/wake/test'; import * as reactEntry from '@crab-dev/wake/test/react'; import {clock, expect, mock, network, test} from '@crab-dev/wake/test'; test('surface', () => { expect(Object.keys(wake).sort()).toEqual(['afterAll','afterEach','beforeAll','beforeEach','clock','describe','expect','it','mock','network','test']); expect(Object.keys(reactEntry).sort()).toEqual(['act','afterAll','afterEach','beforeAll','beforeEach','cleanup','clock','describe','expect','fireEvent','it','mock','network','prettyDOM','render','renderHook','screen','test','userEvent','waitFor','waitForElementToBeRemoved','within']); expect(Object.keys(wake.test).sort()).toEqual(['each','only','skip','todo']); expect(Object.keys(mock).sort()).toEqual(['actual','clearAll','fn','import','isolate','module','replaceProperty','resetAll','restoreAll','spyOn']); expect(Object.keys(clock).sort()).toEqual(['advanceBy','advanceTo','fake','flushMicrotasks','restore','runAll','runNext']); expect(Object.keys(network).sort()).toEqual(['allow','requests','reset','route']) })",
        )
        .unwrap();
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn modern_clock_controls_animation_idle_and_async_timeouts() {
        let fixture = fixture(
            r#"
                import {clock, expect, test} from '@crab-dev/wake/test'
                test('clock', async () => {
                    await clock.fake({now: 100})
                    const values = []
                    requestAnimationFrame(now => values.push(['frame', now]))
                    requestIdleCallback(deadline => values.push(['idle', deadline.didTimeout]))
                    await clock.advanceBy(16)
                    expect(values).toEqual([['idle', false], ['frame', 116]])

                    const order = []
                    queueMicrotask(() => order.push('standalone microtask'))
                    await clock.flushMicrotasks()
                    expect(order).toEqual(['standalone microtask'])

                    setTimeout(() => {
                        order.push('timer')
                        queueMicrotask(() => order.push('timer microtask'))
                    }, 0)
                    await clock.runNext()
                    expect(order).toEqual(['standalone microtask', 'timer', 'timer microtask'])

                    let intervalCalls = 0
                    const interval = setInterval(() => {
                        intervalCalls += 1
                        clearTimeout(interval)
                    }, 0)
                    expect(await clock.runNext()).toBe(true)
                    expect(intervalCalls).toBe(1)
                    expect(await clock.runNext()).toBe(false)
                })
                test('timeout', async () => {
                    await new Promise(resolve => setTimeout(resolve, 5))
                    expect(true).toBe(true)
                }, {timeout: 100})
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn per_case_deadline_terminates_sync_code_and_continues_the_dom_realm() {
        let fixture = fixture(
            r#"
                import {afterEach, beforeEach, expect, test} from '@crab-dev/wake/test'
                const events = []
                let caseNumber = 0
                beforeEach(() => { caseNumber += 1; events.push(`before:${caseNumber}`) }, {timeout: 100})
                afterEach(() => { events.push(`after:${caseNumber}`) }, {timeout: 100})
                test('sync timeout', () => {
                    events.push('test:1')
                    Promise.resolve().then(() => events.push('stale-job'))
                    while (true) {}
                }, {timeout: 25})
                test('same realm continues', () => {
                    expect(events).toEqual(['before:1', 'test:1', 'after:1', 'before:2'])
                }, {timeout: 250})
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();

        assert!(!result.success, "{result:#?}");
        let cases = &result.suites[0].tests;
        assert_eq!(cases.len(), 2, "{result:#?}");
        assert_eq!(cases[0].status, TestStatus::Failed);
        assert_eq!(cases[1].status, TestStatus::Passed, "{result:#?}");
        assert_eq!(
            cases[0].failures[0].code.as_deref(),
            Some("WAKE_TEST_TIMEOUT")
        );
        assert!(cases[0].failures[0].message.contains("25 ms"));
    }

    #[test]
    fn async_never_settling_case_uses_the_same_engine_deadline() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('never settles', () => new Promise(() => {}), {timeout: 30})
                test('continues after async termination', () => expect(true).toBe(true))
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();

        assert!(!result.success, "{result:#?}");
        assert_eq!(
            result.suites[0].tests[0].status,
            TestStatus::Failed,
            "{result:#?}"
        );
        assert_eq!(
            result.suites[0].tests[1].status,
            TestStatus::Passed,
            "{result:#?}"
        );
        assert_eq!(
            result.suites[0].tests[0].failures[0].code.as_deref(),
            Some("WAKE_TEST_TIMEOUT")
        );
        assert!(result.suites[0].tests[0].duration_ms < 1_000);
    }

    #[test]
    fn hook_deadlines_are_phase_scoped_and_before_all_blocks_only_its_suite() {
        let fixture = fixture(
            r#"
                import {afterAll, afterEach, beforeAll, beforeEach, describe, expect, test} from '@crab-dev/wake/test'
                const events = []
                let beforeEachCalls = 0
                let afterEachCalls = 0

                describe('case hooks', () => {
                    beforeEach(() => {
                        beforeEachCalls += 1
                        events.push(`beforeEach:${beforeEachCalls}`)
                        if (beforeEachCalls === 1) while (true) {}
                    }, {timeout: 20})
                    afterEach(() => {
                        afterEachCalls += 1
                        events.push(`afterEach:${afterEachCalls}`)
                        if (afterEachCalls === 2) while (true) {}
                    }, {timeout: 20})
                    test('beforeEach timeout skips callback', () => events.push('must-not-run'))
                    test('afterEach timeout belongs to this case', () => events.push('second-test'))
                    test('later case still runs', () => {
                        expect(events).toEqual([
                            'beforeEach:1', 'afterEach:1',
                            'beforeEach:2', 'second-test', 'afterEach:2',
                            'beforeEach:3',
                        ])
                    })
                })

                describe('blocked suite', () => {
                    beforeAll(() => { while (true) {} }, {timeout: 20})
                    afterAll(() => events.push('blocked-afterAll'), {timeout: 100})
                    test('is skipped', () => events.push('blocked-test'))
                })

                describe('healthy sibling', () => {
                    test('runs after blocked suite', () => {
                        expect(events.includes('blocked-test')).toBe(false)
                        expect(events.includes('blocked-afterAll')).toBe(true)
                    })
                })
            "#,
        );
        let result = run_tests(options(fixture.path())).unwrap();

        assert!(!result.success, "{result:#?}");
        let cases = &result.suites[0].tests;
        assert_eq!(cases.len(), 5, "{result:#?}");
        assert_eq!(cases[0].status, TestStatus::Failed);
        assert_eq!(cases[1].status, TestStatus::Failed);
        assert_eq!(cases[2].status, TestStatus::Passed, "{result:#?}");
        assert_eq!(cases[3].status, TestStatus::Skipped);
        assert_eq!(cases[4].status, TestStatus::Passed, "{result:#?}");
        assert!(
            cases[0].failures[0].message.contains("beforeEach phase")
                && cases[1].failures[0].message.contains("afterEach phase")
        );
        assert!(
            result.suites[0].failures.iter().any(|failure| {
                failure.code.as_deref() == Some("WAKE_TEST_TIMEOUT")
                    && failure.message.contains("beforeAll phase")
            }),
            "{result:#?}"
        );
    }

    #[test]
    fn timed_out_react_cleanup_detaches_owned_roots_before_the_next_case() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-react-cleanup-timeout-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("math.test.ts"),
            r#"
                import React, {useEffect, useState} from 'react'
                import {expect, test} from '@crab-dev/wake/test'
                import {render, screen, userEvent} from '@crab-dev/wake/test/react'

                function BlockingCleanup() {
                    useEffect(() => () => { while (true) {} }, [])
                    return React.createElement('div', null, 'blocking root')
                }

                function HealthyCounter() {
                    const [count, setCount] = useState(0)
                    return React.createElement('button', {onClick: () => setCount(value => value + 1)}, `healthy ${count}`)
                }

                test('cleanup timeout belongs to this case', async () => {
                    await render(React.createElement(BlockingCleanup))
                    expect(screen.getByText('blocking root')).toBeInTheDocument()
                })

                test('next case gets a usable detached DOM', async () => {
                    expect(document.body.textContent).not.toContain('blocking root')
                    await render(React.createElement(HealthyCounter))
                    await userEvent.setup().click(screen.getByRole('button', {name: 'healthy 0'}))
                    expect(screen.getByRole('button', {name: 'healthy 1'})).toBeInTheDocument()
                })
            "#,
        )
        .unwrap();
        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test]\ntimeout_ms = 1000\n",
        )
        .unwrap();
        let result = run_tests(options(fixture.path())).unwrap();

        assert!(!result.success, "{result:#?}");
        assert_eq!(result.suites[0].tests.len(), 2, "{result:#?}");
        assert_eq!(
            result.suites[0].tests[0].status,
            TestStatus::Failed,
            "{result:#?}"
        );
        assert_eq!(
            result.suites[0].tests[1].status,
            TestStatus::Passed,
            "{result:#?}"
        );
        assert!(result.suites[0].tests[0].failures.iter().any(|failure| {
            failure.code.as_deref() == Some("WAKE_TEST_TIMEOUT")
                && failure.message.contains("cleanup phase")
        }));
    }

    #[test]
    fn timer_leaks_are_cancelled_and_follow_the_configured_policy() {
        let fixture = fixture(
            r#"
                import {afterEach, clock, expect, test} from '@crab-dev/wake/test'

                let cleanupHandle
                afterEach(() => {
                    if (cleanupHandle !== undefined) clearInterval(cleanupHandle)
                    cleanupHandle = undefined
                })

                test('real timer leak', () => {
                    setInterval(() => {}, 60_000)
                    expect(true).toBe(true)
                })

                test('fake timer leak', async () => {
                    await clock.fake()
                    setTimeout(() => {}, 60_000)
                    expect(true).toBe(true)
                })

                test('afterEach can clear a timer from the callback phase', () => {
                    cleanupHandle = setInterval(() => {}, 60_000)
                    expect(true).toBe(true)
                })
            "#,
        );

        let warned = run_tests(options(fixture.path())).unwrap();
        assert!(warned.success, "{warned:#?}");
        assert_eq!(warned.leaks.len(), 2, "{warned:#?}");
        assert!(
            warned
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.severity == DiagnosticSeverity::Warning)
        );
        assert!(
            warned
                .leaks
                .iter()
                .any(|leak| leak.description.contains("pending interval"))
        );
        assert!(
            warned
                .leaks
                .iter()
                .any(|leak| leak.description.contains("pending fake timeout"))
        );

        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test]\nleaks = \"error\"\n",
        )
        .unwrap();
        let errored = run_tests(options(fixture.path())).unwrap();
        assert!(!errored.success, "{errored:#?}");
        assert_eq!(errored.leaks.len(), 2, "{errored:#?}");
        assert!(
            errored.suites[0]
                .failures
                .iter()
                .all(|failure| failure.code.as_deref() == Some("WAKE_TEST_LEAK"))
        );

        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test]\nleaks = \"off\"\n",
        )
        .unwrap();
        let ignored = run_tests(options(fixture.path())).unwrap();
        assert!(ignored.success, "{ignored:#?}");
        assert!(ignored.leaks.is_empty(), "{ignored:#?}");
        assert!(
            ignored
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "WAKE_TEST_LEAK")
        );
    }

    #[test]
    fn cancellation_terminates_an_active_v8_instruction_stream() {
        let fixture = fixture(
            "import {test} from '@crab-dev/wake/test'; test('loop', () => { while (true) {} })",
        );
        let cancellation = TestCancellationToken::default();
        let worker_cancellation = cancellation.clone();
        let worker_options = options(fixture.path());
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = sender.send(run_tests_with_cancellation(
                worker_options,
                worker_cancellation,
            ));
        });

        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(cancellation.cancel());
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("cancellation must stop V8 before the suite timeout")
            .unwrap();
        worker.join().unwrap();

        assert!(!result.success, "{result:#?}");
        assert_eq!(result.termination_reason, TestTerminationReason::Cancelled);
        assert!(result.duration_ms < 2_000, "{result:#?}");
    }

    #[test]
    fn network_routes_receive_owned_requests_and_return_web_responses() {
        let fixture = fixture(
            r#"
                import {expect, network, test} from '@crab-dev/wake/test'

                test('route', async () => {
                    const dispose = network.route({method: 'POST', url: /\/users$/}, async request => {
                        expect(request.id).toMatch(/^wake-request-/)
                        expect(request.url).toBeInstanceOf(URL)
                        expect(request.url.href).toBe('http://api.test/users')
                        expect(request.method).toBe('POST')
                        expect(request.headers).toBeInstanceOf(Headers)
                        expect(request.headers.get('x-source')).toBe('wake')
                        expect(new TextDecoder().decode(request.body)).toBe('payload')
                        return {status: 201, headers: {'x-wake': 'yes'}, body: {ok: true}}
                    })
                    const response = await fetch('http://api.test/users', {
                        method: 'POST', headers: {'x-source': 'wake'}, body: 'payload'
                    })
                    expect(response).toBeInstanceOf(Response)
                    expect(response.status).toBe(201)
                    expect(response.headers.get('x-wake')).toBe('yes')
                    expect(await response.json()).toEqual({ok: true})
                    expect(network.requests()).toHaveLength(1)
                    dispose()
                    await expect(fetch('http://api.test/users')).rejects.toThrow(/denied/)
                })
            "#,
        );

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn dom_network_allow_uses_the_bounded_rust_transport_and_rechecks_redirects() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let mut observed = Vec::new();
            for index in 0..2 {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                loop {
                    let count = std::io::Read::read(&mut socket, &mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    let Some(header_end) =
                        request.windows(4).position(|value| value == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_text = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = header_text
                        .lines()
                        .find_map(|line| line.split_once(':'))
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if request.len() >= header_end + 4 + content_length {
                        break;
                    }
                }
                observed.push(String::from_utf8_lossy(&request).into_owned());
                let response = if index == 0 {
                    "HTTP/1.1 302 Found\r\nLocation: /final\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    let body = "transport-ok";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nX-Wake: dom\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                std::io::Write::write_all(&mut socket, response.as_bytes()).unwrap();
            }
            sender.send(observed).unwrap();
        });
        let origin = format!("http://{address}");
        let fixture = fixture(
            &r#"
                import {expect, network, test} from '@crab-dev/wake/test'
                test('allowed transport', async () => {
                    const dispose = network.allow('__ORIGIN__/*')
                    const response = await fetch('__ORIGIN__/start', {method: 'POST', body: 'payload'})
                    expect(response.status).toBe(200)
                    expect(response.headers.get('x-wake')).toBe('dom')
                    expect(response.redirected).toBe(true)
                    expect(response.url).toBe('__ORIGIN__/final')
                    expect(await response.text()).toBe('transport-ok')
                    expect(network.requests()).toHaveLength(2)
                    expect(network.requests()[0].method).toBe('POST')
                    expect(network.requests()[1].method).toBe('GET')
                    dispose()
                })
            "#
            .replace("__ORIGIN__", &origin),
        );

        let result = run_tests(options(fixture.path())).unwrap();
        let observed = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        server.join().unwrap();
        assert!(result.success, "{result:#?}");
        assert!(observed[0].starts_with("POST /start HTTP/1.1"));
        assert!(observed[0].ends_with("payload"));
        assert!(observed[1].starts_with("GET /final HTTP/1.1"));
    }

    #[test]
    fn explicit_module_mocks_await_factories_and_isolate_module_graphs() {
        let fixture = fixture(
            r#"
                import {expect, mock, test} from '@crab-dev/wake/test'

                test('explicit module graph', async () => {
                    let factoryCalls = 0
                    mock.module('./api', async () => {
                        await Promise.resolve()
                        factoryCalls += 1
                        return {loadUser: mock.fn().resolve({name: 'mocked'})}
                    })

                    const app = await mock.import('./app')
                    expect(await app.loadName()).toBe('mocked')
                    expect(factoryCalls).toBe(1)

                    const actual = await mock.actual('./api')
                    expect(await actual.loadUser()).toEqual({name: 'real'})

                    await mock.isolate(async () => {
                        const isolated = await mock.import('./app')
                        expect(await isolated.loadName()).toBe('mocked')
                    })
                    expect(factoryCalls).toBe(2)

                    const cached = await mock.import('./app')
                    expect(await cached.loadName()).toBe('mocked')
                    expect(factoryCalls).toBe(2)
                })
            "#,
        );
        fs::write(
            fixture.path().join("api.ts"),
            "export async function loadUser() { return {name: 'real'} }",
        )
        .unwrap();
        fs::write(
            fixture.path().join("app.ts"),
            "import {loadUser} from './api'; export async function loadName() { return (await loadUser()).name }",
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn module_mocks_use_public_specifiers_for_import_and_require_edges() {
        let fixture = fixture(
            r#"
                import {expect, mock, test} from '@crab-dev/wake/test'

                test('canonical module request identity', async () => {
                    let relativeFactoryCalls = 0
                    mock.module('./relative-api.cjs', async () => {
                        await Promise.resolve()
                        relativeFactoryCalls += 1
                        return {kind: 'relative-mocked'}
                    })
                    const relativeImport = await mock.import('./relative-import')
                    const relativeRequire = await mock.import('./relative-require.cjs')
                    expect(relativeImport.kind).toBe('relative-mocked')
                    expect(relativeRequire.kind).toBe('relative-mocked')
                    expect(relativeFactoryCalls).toBe(1)

                    let bareFactoryCalls = 0
                    mock.module('wake-mock-target', () => {
                        bareFactoryCalls += 1
                        return {kind: 'bare-mocked'}
                    })
                    const bareImport = await mock.import('./bare-import')
                    const bareRequire = await mock.import('./bare-require.cjs')
                    expect(bareImport.kind).toBe('bare-mocked')
                    expect(bareRequire.kind).toBe('bare-mocked')
                    expect(bareFactoryCalls).toBe(1)

                    const actual = await mock.actual('wake-mock-target')
                    expect(actual.kind).toBe('import-real')
                })
            "#,
        );
        fs::write(
            fixture.path().join("relative-api.cjs"),
            "module.exports = {kind: 'relative-real'};",
        )
        .unwrap();
        fs::write(
            fixture.path().join("relative-import.ts"),
            "import {kind} from './relative-api.cjs'; export {kind};",
        )
        .unwrap();
        fs::write(
            fixture.path().join("relative-require.cjs"),
            "module.exports = {kind: require('./relative-api.cjs').kind};",
        )
        .unwrap();
        fs::create_dir_all(fixture.path().join("node_modules/wake-mock-target")).unwrap();
        fs::write(
            fixture
                .path()
                .join("node_modules/wake-mock-target/package.json"),
            r#"{"name":"wake-mock-target","type":"module","exports":{".":{"import":"./import.js","require":"./require.cjs"}}}"#,
        )
        .unwrap();
        fs::write(
            fixture
                .path()
                .join("node_modules/wake-mock-target/import.js"),
            "export const kind = 'import-real';",
        )
        .unwrap();
        fs::write(
            fixture
                .path()
                .join("node_modules/wake-mock-target/require.cjs"),
            "module.exports = {kind: 'require-real'};",
        )
        .unwrap();
        fs::write(
            fixture.path().join("bare-import.ts"),
            "import {kind} from 'wake-mock-target'; export {kind};",
        )
        .unwrap();
        fs::write(
            fixture.path().join("bare-require.cjs"),
            "module.exports = {kind: require('wake-mock-target').kind};",
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn function_spy_and_property_mocks_record_and_restore_owned_state() {
        let fixture = fixture(
            r#"
                import {expect, mock, test} from '@crab-dev/wake/test'

                const target = {value: 1, multiply(value) { return value * 2 }}

                test('records', async () => {
                    const operation = mock.fn(value => value + 1)
                        .implementOnce(value => value + 10)
                        .named('operation')
                    expect(operation(2)).toBe(12)
                    expect(operation(2)).toBe(3)
                    expect(operation).toHaveBeenCalledTimes(2)
                    expect(operation).toHaveBeenNthCalledWith(1, 2)
                    expect(operation).toHaveLastReturnedWith(3)
                    expect(operation.name).toBe('operation')

                    const resolved = mock.fn().resolve('ready')
                    await expect(resolved()).resolves.toBe('ready')
                    const rejected = mock.fn().reject(new Error('offline'))
                    await expect(rejected()).rejects.toThrow('offline')

                    const spy = mock.spyOn(target, 'multiply')
                    expect(target.multiply(4)).toBe(8)
                    expect(spy).toHaveBeenCalledWith(4)
                    const replaced = mock.replaceProperty(target, 'value', 9)
                    expect(target.value).toBe(9)
                    replaced.replace(11)
                    expect(target.value).toBe(11)
                })

                test('automatic restore', () => {
                    expect(target.multiply(4)).toBe(8)
                    expect(target.value).toBe(1)
                })
            "#,
        );

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn browser_network_bridge_serializes_one_owned_decision_per_request() {
        let fixture = fixture(
            r#"
                import {expect, network, test} from '@crab-dev/wake/test'

                test('owned bridge', async () => {
                    let calls = 0
                    const dispose = network.route({method: 'POST', url: 'https://assets.test/data'}, request => {
                        calls += 1
                        expect(request.id).toBe('wake-request-1')
                        expect(request.url.href).toBe('https://assets.test/data')
                        expect(request.headers.get('x-a')).toBe('first')
                        expect(new TextDecoder().decode(request.body)).toBe('payload')
                        return new Response(new Uint8Array([0, 1, 255]), {
                            status: 206,
                            statusText: 'Partial Content',
                            headers: {'x-z': 'last', 'x-a': 'first'},
                        })
                    })
                    const decision = JSON.parse(await globalThis.__wakeHandleBrowserNetworkRequest({
                        url: 'https://assets.test/data',
                        method: 'post',
                        headers: [{name: 'x-z', value: 'last'}, {name: 'x-a', value: 'first'}],
                        body: [112, 97, 121, 108, 111, 97, 100],
                        resourceType: 'Fetch',
                    }))
                    expect(decision).toEqual({
                        action: 'fulfill',
                        status: 206,
                        statusText: 'Partial Content',
                        headers: [{name: 'x-a', value: 'first'}, {name: 'x-z', value: 'last'}],
                        body: [0, 1, 255],
                    })
                    expect(calls).toBe(1)
                    expect(network.requests()).toHaveLength(1)
                    dispose()
                    const denied = JSON.parse(await globalThis.__wakeHandleBrowserNetworkRequest({
                        url: 'https://assets.test/denied', method: 'GET', headers: [], body: null,
                        resourceType: 'Image',
                    }))
                    expect(denied).toEqual({action: 'fail', errorReason: 'BlockedByClient'})
                    const configured = JSON.parse(await globalThis.__wakeHandleBrowserNetworkRequest({
                        url: 'https://allowed.test/configured', method: 'GET', headers: [], body: null,
                        resourceType: 'Stylesheet',
                    }))
                    expect(configured).toEqual({action: 'continue'})
                    expect(network.requests()).toHaveLength(3)
                })
            "#,
        );
        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test.network]\nallow_hosts = ['allowed.test']\n",
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn precise_coverage_tracks_dependency_modules_and_writes_owned_reports() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                import {classify} from './value'
                test('covered branch', () => expect(classify(2)).toBe('🦀'))
            "#,
        );
        fs::write(
            fixture.path().join("value.ts"),
            "export function classify(value: number) {\n    const marker: string = '🦀'\n    if (value > 0) {\n        return marker\n    }\n    return 'other'\n}\n",
        )
        .unwrap();
        fs::write(
            fixture.path().join("wake.config.toml"),
            "[test.coverage]\nenabled = true\nreporters = ['text', 'json', 'lcov', 'html']\n",
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        let coverage = result.coverage.as_ref().expect("coverage result");
        assert_eq!(coverage.files.len(), 1, "{coverage:#?}");
        assert_eq!(coverage.files[0].path, "value.ts");
        assert_eq!(coverage.files[0].metrics.lines.total, 5, "{coverage:#?}");
        assert_eq!(coverage.files[0].metrics.lines.covered, 4, "{coverage:#?}");
        assert_eq!(
            coverage.files[0].metrics.functions.total, 1,
            "{coverage:#?}"
        );
        assert_eq!(
            coverage.files[0].metrics.functions.covered, 1,
            "{coverage:#?}"
        );
        assert_eq!(coverage.files[0].metrics.blocks.total, 1, "{coverage:#?}");
        assert_eq!(coverage.files[0].metrics.blocks.covered, 0, "{coverage:#?}");
        assert_eq!(result.artifacts.len(), 4, "{result:#?}");
        let text = fs::read_to_string(fixture.path().join("coverage/coverage.txt")).unwrap();
        assert!(text.contains("All files"), "{text}");
        assert!(text.contains("value.ts"), "{text}");
        let report = fixture.path().join("coverage/wake-coverage.json");
        assert!(report.is_file());
        let report: CoverageResult =
            serde_json::from_str(&fs::read_to_string(report).unwrap()).unwrap();
        assert_eq!(report.files[0].path, "value.ts");
        let lcov = fs::read_to_string(fixture.path().join("coverage/lcov.info")).unwrap();
        assert!(lcov.contains("SF:value.ts\n"), "{lcov}");
        assert!(lcov.contains("FNDA:1,wake_fn_0\n"), "{lcov}");
        assert!(lcov.contains("BRDA:"), "{lcov}");
        assert!(lcov.contains("DA:3,"), "{lcov}");
        let html = fs::read_to_string(fixture.path().join("coverage/index.html")).unwrap();
        assert!(html.contains("<title>Wake coverage</title>"), "{html}");
        assert!(html.contains("const marker"), "{html}");
        assert!(html.contains("class=\"line uncovered\""), "{html}");
    }

    fn branch_coverage_fixture(second_branch: bool) -> tempfile::TempDir {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                import {choose} from './value'
                test('first branch', () => expect(choose(true).props.children).toBe('🦀'))
            "#,
        );
        fs::write(
            fixture.path().join("other.test.ts"),
            format!(
                r#"
                    import {{expect, test}} from '@crab-dev/wake/test'
                    import {{choose}} from './value'
                    test('second branch', () => expect(choose({second_branch}).props.children).toBe({expected:?}))
                "#,
                expected = if second_branch { "🦀" } else { "other" },
            ),
        )
        .unwrap();
        fs::write(
            fixture.path().join("value.tsx"),
            "export function choose(flag: boolean) {\n    const marker: string = '🦀'\n    if (flag) {\n        return <span>{marker}</span>\n    }\n    return <span>other</span>\n}\n",
        )
        .unwrap();
        let react = fixture.path().join("node_modules/react");
        fs::create_dir_all(&react).unwrap();
        fs::write(
            react.join("package.json"),
            r#"{"name":"react","exports":{"./jsx-dev-runtime":"./jsx-dev-runtime.js"}}"#,
        )
        .unwrap();
        fs::write(
            react.join("jsx-dev-runtime.js"),
            "export function jsxDEV(type, props) { return {type, props} } export const Fragment = Symbol.for('wake.fragment')",
        )
        .unwrap();
        fs::write(
            fixture.path().join("wake.config.toml"),
            r#"
                [test.coverage]
                enabled = true
                reporters = ["json"]

                [test.coverage.threshold]
                lines = 100
                blocks = 100
            "#,
        )
        .unwrap();
        fixture
    }

    #[test]
    fn coverage_identity_deduplicates_the_same_tsx_branch_across_suites() {
        let fixture = branch_coverage_fixture(true);
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success, "{result:#?}");
        let coverage = result.coverage.as_ref().expect("coverage result");
        assert_eq!(coverage.files.len(), 1, "{coverage:#?}");
        let metrics = &coverage.files[0].metrics;
        assert_eq!((metrics.lines.covered, metrics.lines.total), (4, 5));
        assert_eq!((metrics.functions.covered, metrics.functions.total), (1, 1));
        assert_eq!((metrics.blocks.covered, metrics.blocks.total), (0, 1));
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("global lines coverage"))
        );
    }

    #[test]
    fn coverage_identity_unions_different_tsx_branches_before_reports_and_thresholds() {
        let fixture = branch_coverage_fixture(false);
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        let coverage = result.coverage.as_ref().expect("coverage result");
        assert_eq!(coverage.files.len(), 1, "{coverage:#?}");
        assert_eq!(coverage.files[0].path, "value.tsx");
        let metrics = &coverage.files[0].metrics;
        assert_eq!((metrics.lines.covered, metrics.lines.total), (5, 5));
        assert_eq!((metrics.functions.covered, metrics.functions.total), (1, 1));
        assert_eq!((metrics.blocks.covered, metrics.blocks.total), (2, 2));
        assert!(result.diagnostics.is_empty(), "{result:#?}");

        let report: CoverageResult = serde_json::from_str(
            &fs::read_to_string(fixture.path().join("coverage/wake-coverage.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(report.summary, coverage.summary);
        assert_eq!(report.files, coverage.files);
    }

    #[test]
    fn coverage_thresholds_fail_the_run_without_rejecting_execution() {
        let fixture = fixture(
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                import {classify} from './value'
                test('covered branch', () => expect(classify(2)).toBe('positive'))
            "#,
        );
        fs::write(
            fixture.path().join("value.ts"),
            "export function classify(value: number) { return value > 0 ? 'positive' : 'other' }",
        )
        .unwrap();
        fs::write(
            fixture.path().join("wake.config.toml"),
            r#"
                [test.coverage]
                enabled = true

                [test.coverage.threshold]
                blocks = 100

                [[test.coverage.per_file]]
                pattern = "value.ts"
                blocks = 100
            "#,
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success, "{result:#?}");
        assert_eq!(result.counts.suites.failed, 0, "{result:#?}");
        assert!(result.coverage.is_some());
        let diagnostics = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "WAKE_TEST_COVERAGE")
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 2, "{diagnostics:#?}");
        assert!(diagnostics[0].message.contains("global blocks coverage"));
        assert!(diagnostics[1].message.contains("value.ts blocks coverage"));
    }

    #[test]
    fn forbid_only_and_react_test_id_configuration_are_enforced() {
        let focused =
            fixture("import {test} from '@crab-dev/wake/test'; test.only('focused', () => {})");
        fs::write(
            focused.path().join("wake.config.toml"),
            "[test]\nforbid_only = true\n",
        )
        .unwrap();
        let focused_result = run_tests(options(focused.path())).unwrap();
        assert!(!focused_result.success);
        assert!(
            focused_result.suites[0].failures[0]
                .message
                .contains("Focused tests are forbidden")
        );

        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let react = tempfile::Builder::new()
            .prefix("wake-react-config-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(react.path().join("package.json"), "{}").unwrap();
        fs::write(
            react.path().join("wake.config.toml"),
            "[test.react]\ntest_id_attribute = \"data-wake-id\"\n",
        )
        .unwrap();
        fs::write(
            react.path().join("react.test.ts"),
            r#"
                import React from 'react'
                import {expect, render, screen, test} from '@crab-dev/wake/test/react'
                test('custom id', async () => {
                    await render(React.createElement('div', {'data-wake-id': 'answer'}, '42'))
                    expect(screen.getByTestId('answer')).toHaveTextContent('42')
                })
            "#,
        )
        .unwrap();
        let result = run_tests(options(react.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn react_act_warnings_follow_off_warn_and_error_policy() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-react-act-warning-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("react.test.ts"),
            r#"
                import React, {useState} from 'react'
                import {render, test} from '@crab-dev/wake/test/react'

                let update
                function Counter() {
                    const [value, setValue] = useState(0)
                    update = setValue
                    return React.createElement('output', null, String(value))
                }

                test('unwrapped update', async () => {
                    await render(React.createElement(Counter))
                    update(1)
                    await Promise.resolve()
                })
            "#,
        )
        .unwrap();

        let run = |policy: &str| {
            fs::write(
                fixture.path().join("wake.config.toml"),
                format!("[test.react]\nact_warnings = \"{policy}\"\n"),
            )
            .unwrap();
            run_tests(options(fixture.path())).unwrap()
        };

        let errored = run("error");
        assert!(!errored.success, "{errored:#?}");
        assert!(errored.suites[0].tests[0].failures.iter().any(|failure| {
            failure.code.as_deref() == Some("WAKE_TEST_ACT")
                && failure.message.contains("not wrapped in act")
        }));
        assert!(errored.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "WAKE_TEST_ACT" && diagnostic.severity == DiagnosticSeverity::Error
        }));

        let warned = run("warn");
        assert!(warned.success, "{warned:#?}");
        assert!(warned.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == "WAKE_TEST_ACT" && diagnostic.severity == DiagnosticSeverity::Warning
        }));

        let ignored = run("off");
        assert!(ignored.success, "{ignored:#?}");
        assert!(
            ignored
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "WAKE_TEST_ACT"),
            "{ignored:#?}"
        );
    }

    #[test]
    fn react_19_create_root_and_async_act_run_in_the_fast_dom_realm() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-react-spike-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("react.test.ts"),
            r#"
                import React, {act} from 'react'
                import {createRoot} from 'react-dom/client'
                import {expect, test} from '@crab-dev/wake/test'

                test('React 19 DOM', async () => {
                    const container = document.createElement('div')
                    document.body.appendChild(container)
                    const root = createRoot(container)
                    await act(async () => {
                        root.render(React.createElement('button', {type: 'button'}, 'Wake React'))
                    })
                    expect(container.querySelector('button').textContent).toBe('Wake React')
                    await act(async () => root.unmount())
                    expect(container.childNodes.length).toBe(0)
                })
            "#,
        )
        .unwrap();
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn react_entry_renders_queries_interacts_and_cleans_up() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-react-entry-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("react.test.ts"),
            r#"
                import React, {useState} from 'react'
                import {cleanup, expect, render, renderHook, screen, test, userEvent} from '@crab-dev/wake/test/react'

                function Counter() {
                    const [count, setCount] = useState(0)
                    return React.createElement(
                        'button',
                        {type: 'button', onClick: () => setCount(value => value + 1)},
                        `Count ${count}`,
                    )
                }

                function Wrapper({children}) {
                    return React.createElement('section', {'data-wake-wrapper': 'true'}, children)
                }

                test('React entry', async () => {
                    await render(React.createElement(Counter))
                    const button = screen.getByRole('button', {name: 'Count 0'})
                    await userEvent.setup().click(button)
                    expect(screen.getByRole('button', {name: 'Count 1'})).toBeInTheDocument()
                })

                test('previous roots are gone', () => {
                    expect(document.body.querySelectorAll('button').length).toBe(0)
                })

                test('render and renderHook apply one wrapper inside strict mode', async () => {
                    await render(React.createElement('output', null, 'wrapped'), {
                        wrapper: Wrapper,
                        strict: true,
                    })
                    expect(document.body.querySelectorAll('[data-wake-wrapper]').length).toBe(1)
                    expect(screen.getByText('wrapped')).toBeInTheDocument()
                    await cleanup()

                    const hook = await renderHook(() => 'hook value', {
                        wrapper: Wrapper,
                        strict: true,
                    })
                    expect(hook.result.current).toBe('hook value')
                    expect(document.body.querySelectorAll('[data-wake-wrapper]').length).toBe(1)
                })
            "#,
        )
        .unwrap();
        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        assert_eq!(result.environment.react.as_deref(), Some("19.2.8"));
        assert_eq!(result.environment.react_dom.as_deref(), Some("19.2.8"));
    }

    #[test]
    fn official_react_testing_library_pure_matches_wake_adapter_basics() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-react-library-conformance-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("react.test.ts"),
            r#"
                import React from 'react'
                import {
                    cleanup as libraryCleanup,
                    render as libraryRender,
                    screen as libraryScreen,
                } from '@testing-library/react/pure'
                import {
                    cleanup as wakeCleanup,
                    expect,
                    render as wakeRender,
                    screen as wakeScreen,
                    test,
                } from '@crab-dev/wake/test/react'

                const view = () => React.createElement(
                    'button',
                    {type: 'button', 'aria-label': 'Save changes'},
                    'Save',
                )

                test('render, screen, and cleanup agree', async () => {
                    const libraryResult = libraryRender(view())
                    const libraryMarkup = libraryResult.container.innerHTML
                    expect(libraryScreen.getByRole('button', {name: 'Save changes'}).textContent).toBe('Save')
                    libraryCleanup()
                    expect(document.body.querySelector('button')).toBe(null)

                    const wakeResult = await wakeRender(view())
                    expect(wakeScreen.getByRole('button', {name: 'Save changes'}).textContent).toBe('Save')
                    expect(wakeResult.container.innerHTML).toBe(libraryMarkup)
                    await wakeCleanup()
                    expect(document.body.querySelector('button')).toBe(null)
                })
            "#,
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
    }

    #[test]
    fn react_entry_rejects_mismatched_project_versions_before_execution() {
        let fixture = fixture(
            "import {test} from '@crab-dev/wake/test/react'; test('never starts', () => {})",
        );
        for (package, version) in [("react", "19.2.8"), ("react-dom", "19.2.7")] {
            let directory = fixture.path().join("node_modules").join(package);
            fs::create_dir_all(&directory).unwrap();
            fs::write(
                directory.join("package.json"),
                serde_json::json!({"name": package, "version": version}).to_string(),
            )
            .unwrap();
        }
        let error = run_tests(options(fixture.path())).unwrap_err();
        assert_eq!(error.code(), "WAKE_TEST_REACT_VERSION");
        assert!(error.to_string().contains("react-dom \"19.2.7\""));
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn chromium_and_dom_map_the_same_failure_to_the_same_original_ts_source() {
        let fixture = fixture(concat!(
            "import {expect, test} from '@crab-dev/wake/test'\n",
            "test('same source', async () => {\n",
            "  await Promise.resolve()\n",
            "  const marker: string = '🦀'; expect(marker).toBe('other')\n",
            "})\n",
        ));
        let mut dom_options = options(fixture.path());
        dom_options.environment = Some("dom".to_string());
        let dom = run_tests(dom_options).unwrap();

        let mut browser_options = options(fixture.path());
        browser_options.environment = Some("browser".to_string());
        let browser = run_tests(browser_options).unwrap();

        let dom_case = &dom.suites[0].tests[0];
        let browser_case = &browser.suites[0].tests[0];
        let dom_failure = &dom_case.failures[0];
        let browser_failure = &browser_case.failures[0];
        assert_eq!(
            dom_failure.location,
            Some(TestLocation {
                path: "math.test.ts".to_string(),
                line: 4,
                column: 39,
                end_line: None,
                end_column: None,
            })
        );
        assert_eq!(browser_failure.location, dom_failure.location);
        assert_eq!(browser_case.location, dom_case.location);
        assert_eq!(browser_failure.code, dom_failure.code);
        assert_eq!(browser_failure.diff, dom_failure.diff);
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn chromium_executes_the_same_wake_graph_and_returns_browser_metadata() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let allow_url = format!("http://{}/allowed", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + std::time::Duration::from_secs(15);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                            .unwrap();
                        let mut request = [0_u8; 4096];
                        let read = std::io::Read::read(&mut stream, &mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..read]);
                        assert!(request.starts_with("GET /allowed "), "{request}");
                        std::io::Write::write_all(
                            &mut stream,
                            b"HTTP/1.1 200 OK\r\ncontent-type: text/plain\r\naccess-control-allow-origin: *\r\ncontent-length: 7\r\nconnection: close\r\n\r\nallowed",
                        )
                        .unwrap();
                        return;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "browser never continued allowed fetch"
                        );
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("could not accept browser request: {error}"),
                }
            }
        });
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-browser-conformance-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("value.ts"),
            "export const answer: number = 6 * 7",
        )
        .unwrap();
        let source = r#"
                import React, {useState} from 'react'
                import {expect, network, test} from '@crab-dev/wake/test'
                import {render, screen, userEvent} from '@crab-dev/wake/test/react'
                import {answer} from './value'
                globalThis.__wakeInputSuite = 'first'

                function InteractiveForm() {
                    const [value, setValue] = useState('')
                    const [clicks, setClicks] = useState(0)
                    const [doubleClicks, setDoubleClicks] = useState(0)
                    const [hovered, setHovered] = useState(false)
                    const [checked, setChecked] = useState(false)
                    const [selected, setSelected] = useState('alpha')
                    const [fileName, setFileName] = useState('')
                    return React.createElement('main', null,
                        React.createElement('button', {
                            type: 'button',
                            onClick: event => { globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted); setClicks(count => count + 1) },
                            onDoubleClick: event => { globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted); setDoubleClicks(count => count + 1) },
                            onPointerDown: event => globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted),
                            onMouseOver: event => { globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted); setHovered(true) },
                        }, `Clicks ${clicks}; doubles ${doubleClicks}; hovered ${hovered}`),
                        React.createElement('input', {
                            'aria-label': 'Primary', value,
                            onFocus: event => globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted),
                            onKeyDown: event => globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted),
                            onChange: event => { globalThis.__wakeTrustedEvents.push(event.nativeEvent.isTrusted); setValue(event.target.value) },
                        }),
                        React.createElement('input', {'aria-label': 'Secondary'}),
                        React.createElement('input', {
                            type: 'checkbox', 'aria-label': 'Enabled', checked,
                            onChange: event => setChecked(event.target.checked),
                        }),
                        React.createElement('select', {
                            'aria-label': 'Choice', value: selected,
                            onChange: event => setSelected(event.target.value),
                        },
                            React.createElement('option', {value: 'alpha'}, 'Alpha'),
                            React.createElement('option', {value: 'beta'}, 'Beta'),
                        ),
                        React.createElement('input', {
                            type: 'file', 'aria-label': 'Upload',
                            onChange: event => setFileName(event.target.files[0]?.name || ''),
                        }),
                        React.createElement('output', null, `${value}|${checked}|${selected}|${fileName}`),
                    )
                }

                test('browser graph and network', async () => {
                    const button = document.createElement('button')
                    button.textContent = 'Wake'
                    document.body.appendChild(button)
                    expect(answer).toBe(42)
                    expect(document.querySelector('button').textContent).toBe('Wake')

                    let handlerCalls = 0
                    network.route({method: 'POST', url: 'https://wake.test/api'}, request => {
                        handlerCalls += 1
                        expect(new TextDecoder().decode(request.body)).toBe('payload')
                        return new Response(JSON.stringify({ok: true}), {
                            status: 201,
                            statusText: 'Created',
                            headers: {
                                'access-control-allow-origin': '*',
                                'access-control-expose-headers': 'x-wake',
                                'content-type': 'application/json',
                                'x-wake': 'route',
                            },
                        })
                    })
                    const response = await fetch('https://wake.test/api', {method: 'POST', body: 'payload'})
                    expect(response.status).toBe(201)
                    expect(response.statusText).toBe('Created')
                    expect(response.headers.get('x-wake')).toBe('route')
                    expect(await response.json()).toEqual({ok: true})

                    network.route('https://wake.test/pixel.png', () => {
                        handlerCalls += 1
                        const bytes = Uint8Array.from(atob('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII='), value => value.charCodeAt(0))
                        return {status: 200, headers: {'content-type': 'image/png'}, body: bytes}
                    })
                    const image = new Image()
                    const loaded = new Promise((resolve, reject) => {
                        image.onload = resolve
                        image.onerror = () => reject(new Error('mock image did not load'))
                    })
                    image.src = 'https://wake.test/pixel.png'
                    document.body.appendChild(image)
                    await loaded
                    expect(image.complete).toBe(true)

                    network.route('https://wake.test/theme.css', () => {
                        handlerCalls += 1
                        return {
                            headers: {'content-type': 'text/css'},
                            body: '.wake-network-style { color: rgb(1, 2, 3); }',
                        }
                    })
                    const stylesheet = document.createElement('link')
                    stylesheet.rel = 'stylesheet'
                    const styled = document.createElement('div')
                    styled.className = 'wake-network-style'
                    document.body.append(stylesheet, styled)
                    const stylesheetLoaded = new Promise((resolve, reject) => {
                        stylesheet.onload = resolve
                        stylesheet.onerror = () => reject(new Error('mock stylesheet did not load'))
                    })
                    stylesheet.href = 'https://wake.test/theme.css'
                    await stylesheetLoaded
                    expect(getComputedStyle(styled).color).toBe('rgb(1, 2, 3)')

                    await expect(fetch('https://wake.test/denied')).rejects.toThrow()
                    network.allow('__ALLOW_URL__')
                    const allowed = await fetch('__ALLOW_URL__')
                    expect(await allowed.text()).toBe('allowed')
                    expect(handlerCalls).toBe(3)
                    expect(network.requests()).toHaveLength(5)
                    expect(network.requests().filter(request => request.url.href === 'https://wake.test/api')).toHaveLength(1)
                    button.remove()
                })

                test('typed CDP user input drives React and browser defaults', async () => {
                    globalThis.__wakeTrustedEvents = []
                    await render(React.createElement(InteractiveForm))
                    const user = userEvent.setup()
                    const button = screen.getByRole('button', {name: 'Clicks 0; doubles 0; hovered false'})
                    const primary = screen.getByLabelText('Primary')
                    const secondary = screen.getByLabelText('Secondary')
                    const checkbox = screen.getByLabelText('Enabled')
                    const select = screen.getByLabelText('Choice')
                    const upload = screen.getByLabelText('Upload')

                    await user.click(button)
                    expect(button).toHaveTextContent('Clicks 1')
                    await user.dblClick(button)
                    expect(button).toHaveTextContent('Clicks 3')
                    expect(button).toHaveTextContent('doubles 1')
                    await user.hover(button)
                    expect(button).toHaveTextContent('hovered true')

                    network.route('https://wake.test/input-overlap', () => new Response('overlap', {
                        headers: {'access-control-allow-origin': '*'},
                    }))
                    const [overlap] = await Promise.all([
                        fetch('https://wake.test/input-overlap'),
                        user.click(button),
                    ])
                    expect(await overlap.text()).toBe('overlap')
                    expect(button).toHaveTextContent('Clicks 4')

                    await user.type(primary, 'ab🦀')
                    expect(primary).toHaveValue('ab🦀')
                    await user.clear(primary)
                    expect(primary).toHaveValue('')
                    await user.type(primary, 'x')
                    await user.keyboard('{Backspace}q')
                    expect(primary).toHaveValue('q')
                    await user.tab()
                    expect(secondary).toHaveFocus()

                    await user.click(checkbox)
                    expect(checkbox).toBeChecked()
                    await user.selectOptions(select, 'beta')
                    expect(select).toHaveValue('beta')
                    await user.upload(upload, new File(['Wake'], 'wake.txt', {type: 'text/plain'}))
                    expect(upload.files[0].name).toBe('wake.txt')
                    expect(await upload.files[0].text()).toBe('Wake')
                    expect(document.querySelector('output')).toHaveTextContent('q|true|beta|wake.txt')

                    expect(globalThis.__wakeTrustedEvents.length > 0).toBe(true)
                    expect(globalThis.__wakeTrustedEvents.every(Boolean)).toBe(true)

                    const hidden = document.createElement('button')
                    hidden.hidden = true
                    document.body.appendChild(hidden)
                    let hiddenFailure
                    try { await user.click(hidden) } catch (error) { hiddenFailure = error }
                    expect(hiddenFailure.code).toBe('WAKE_TEST_BROWSER')
                    expect(hiddenFailure.message).toContain('not visible')

                    const detached = document.createElement('button')
                    let detachedFailure
                    try { await user.click(detached) } catch (error) { detachedFailure = error }
                    expect(detachedFailure.code).toBe('WAKE_TEST_BROWSER')
                    expect(detachedFailure.message).toContain('detached')

                    const noLayout = document.createElement('button')
                    noLayout.style.cssText = 'position:fixed;width:0;height:0;padding:0;border:0'
                    document.body.appendChild(noLayout)
                    let layoutFailure
                    try { await user.click(noLayout) } catch (error) { layoutFailure = error }
                    expect(layoutFailure.code).toBe('WAKE_TEST_BROWSER')
                    expect(layoutFailure.message).toContain('layout box')
                })
            "#
        .replace("__ALLOW_URL__", &allow_url);
        fs::write(fixture.path().join("math.browser.test.ts"), source).unwrap();
        fs::write(
            fixture.path().join("isolation.browser.test.ts"),
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('browser suite owns a fresh context and realm', () => {
                    expect(globalThis.__wakeInputSuite).toBe(undefined)
                    expect(document.body.childElementCount).toBe(0)
                })
            "#,
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(result.success, "{result:#?}");
        server.join().unwrap();
        assert_eq!(result.environment.kind, "browser");
        let browser = result.environment.browser.expect("browser metadata");
        assert!(!browser.version.is_empty());
        assert!(browser.headless);
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn chromium_timeout_is_case_scoped_and_skips_the_contaminated_context() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-browser-timeout-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        fs::write(
            fixture.path().join("timeout.browser.test.ts"),
            r#"
                import {expect, test} from '@crab-dev/wake/test'
                test('completed before timeout', () => expect(6 * 7).toBe(42))
                test('sync browser timeout', () => {
                    Promise.resolve().then(() => { globalThis.__wakeStaleBrowserJob = true })
                    while (true) {}
                }, {timeout: 50})
                test('must be skipped with the context', () => {
                    throw new Error('the contaminated BrowserContext was reused')
                })
            "#,
        )
        .unwrap();

        let result = run_tests(options(fixture.path())).unwrap();
        assert!(!result.success, "{result:#?}");
        let cases = &result.suites[0].tests;
        assert_eq!(cases.len(), 3, "{result:#?}");
        assert_eq!(cases[0].status, TestStatus::Passed, "{result:#?}");
        assert_eq!(cases[1].status, TestStatus::Failed, "{result:#?}");
        assert_eq!(cases[2].status, TestStatus::Skipped, "{result:#?}");
        assert_eq!(
            cases[1].failures[0].code.as_deref(),
            Some("WAKE_TEST_TIMEOUT")
        );
        assert!(cases[1].failures[0].message.contains("50 ms"));
        assert!(result.suites[0].failures.is_empty(), "{result:#?}");
    }

    #[test]
    #[ignore = "requires an installed system Chromium browser"]
    fn chromium_screenshot_matcher_adds_matches_diffs_and_updates_profiled_baselines() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let fixture = tempfile::Builder::new()
            .prefix("wake-screenshot-conformance-")
            .tempdir_in(repository.join("target"))
            .unwrap();
        fs::write(fixture.path().join("package.json"), "{}").unwrap();
        let path = fixture.path().join("visual.browser.test.ts");
        let source = |color: &str| {
            format!(
                r#"
                    import React from 'react'
                    import {{expect, test}} from '@crab-dev/wake/test'
                    import {{render, screen}} from '@crab-dev/wake/test/react'
                    test('visual card', async () => {{
                        await render(React.createElement('div', {{
                            'data-testid': 'card',
                            style: {{position: 'fixed', left: 20, top: 20, width: 40, height: 90, background: '{color}'}},
                        }}))
                        const card = screen.getByTestId('card')
                        requestAnimationFrame(() => requestAnimationFrame(() => {{ card.style.width = '160px' }}))
                        await expect(card).toMatchScreenshot('card')
                    }})
                "#
            )
        };
        fs::write(&path, source("rgb(200, 10, 20)")).unwrap();

        let added = run_tests(options(fixture.path())).unwrap();
        assert!(added.success, "{added:#?}");
        assert_eq!(added.snapshot.added, 1);
        let baseline = added
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "screenshot-baseline")
            .unwrap();
        assert!(Path::new(&baseline.path).is_file());
        let baseline_png = fs::read(&baseline.path).unwrap();
        assert_eq!(&baseline_png[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(
            u32::from_be_bytes(baseline_png[16..20].try_into().unwrap()),
            160,
            "the element clip must be measured after the second animation frame"
        );
        assert_eq!(
            u32::from_be_bytes(baseline_png[20..24].try_into().unwrap()),
            90
        );
        let profile_hash = baseline.metadata["profileHash"].clone();
        assert_eq!(baseline.metadata["reducedMotion"], REDUCED_MOTION);

        let matched = run_tests(options(fixture.path())).unwrap();
        assert!(matched.success, "{matched:#?}");
        assert_eq!(matched.snapshot.matched, 1);
        assert_eq!(matched.artifacts[0].metadata["profileHash"], profile_hash);
        assert_eq!(
            matched.artifacts[0].metadata["reducedMotion"],
            REDUCED_MOTION
        );

        fs::write(&path, source("rgb(10, 20, 200)")).unwrap();
        let mut mismatch_options = options(fixture.path());
        mismatch_options.update_snapshots = Some("none".to_string());
        let mismatched = run_tests(mismatch_options).unwrap();
        assert!(!mismatched.success, "{mismatched:#?}");
        assert_eq!(mismatched.snapshot.unmatched, 1);
        assert_eq!(
            mismatched.suites[0].tests[0].failures[0].code.as_deref(),
            Some("WAKE_TEST_SNAPSHOT")
        );
        let visual_diff = mismatched.suites[0].tests[0].failures[0]
            .diff
            .as_ref()
            .unwrap();
        assert!(
            visual_diff
                .received
                .as_deref()
                .is_some_and(|path| path.ends_with(".received.png"))
        );
        assert!(
            visual_diff
                .unified
                .as_deref()
                .is_some_and(|path| path.contains(".diff.html"))
        );
        for kind in [
            "screenshot-baseline",
            "screenshot-received",
            "screenshot-diff",
        ] {
            let artifact = mismatched
                .artifacts
                .iter()
                .find(|artifact| artifact.kind == kind)
                .unwrap_or_else(|| panic!("missing {kind} artifact: {mismatched:#?}"));
            assert!(Path::new(&artifact.path).is_file());
        }

        let mut update_options = options(fixture.path());
        update_options.update_snapshots = Some("all".to_string());
        let updated = run_tests(update_options).unwrap();
        assert!(updated.success, "{updated:#?}");
        assert_eq!(updated.snapshot.updated, 1);
        assert_eq!(updated.artifacts.len(), 1);
        assert_eq!(updated.artifacts[0].kind, "screenshot-baseline");
    }
}
