//! wake_dev_server — Dev Server + Live Reload（DESIGN §7 / PLAN Phase 5）。
//!
//! `wake dev <root>`：以 actix-web 起 HTTP 服务，从**内存**服务增量打包产物；notify 监听源码变更
//! （75ms 静默窗口防抖）→ retained `BuildSession` 增量重建 → 经 WebSocket 广播事件 → 浏览器 client runtime
//! 触发整页刷新 / 显示错误 overlay / 断连自动重连。SPA fallback：未知路径回退到 HTML。
//!
//! 普通应用更新明确是 Live Reload：Wake 不提供 `import.meta.hot`、accept/dispose、React Fast
//! Refresh 或状态保持式模块替换。Federation 的版本化 types-only / isolated-remount /
//! full-reload 更新是独立协议，不能作为普通应用 HMR 能力的证据。
//!
//! 线程模型：**监听线程独占 generation-owned retained `BuildSession`**（由 typed
//! options 创建并在该线程失效 + 重建），
//! 只把产物 `String` 经 `RwLock` 跨线程共享 → 无需 session 满足 `Send`。HTTP 处理器只读共享产物。

mod federation;

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex, RwLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use actix_web::{App, HttpRequest, HttpResponse, HttpServer, web};
use futures_util::StreamExt as _;
use notify::{RecursiveMode, Watcher};
use serde::Serialize;
use tokio::sync::{broadcast, watch};

pub use federation::{
    FEDERATION_BUILD_ID_PLACEHOLDER, FederationBuildOptions, FederationExposeBuild,
    FederationSharedBuild, FederationTypeEmitter, FederationTypeGeneration, FederationTypeOutput,
};
use wake_bundler::{
    BuildGeneration, BuildOptions as BundlerBuildOptions, BuildRequest, BuildSession,
    FederationBuildPlan, FederationEntryExport, JsxOptions, ResolveOptions,
};
use wake_common::{Diagnostic, FileSystem, OsFileSystem};
use wake_ecma_transform::TargetEnv;
pub use wake_federation_contract::{
    DevLeaseMessage, DevLeaseReloadReason, DevUpdate, DevUpdateAction,
    FEDERATION_DEV_LEASE_SCHEMA_VERSION, FEDERATION_DEV_MAX_BUILD_LEASES,
};

// —— 终端着色（tty + 非 NO_COLOR 时启用）——
const RESET: &str = "\x1b[0m";
const WATCH_SETTLE_QUIET: Duration = Duration::from_millis(75);
const LIVE_RELOAD_ENDPOINT: &str = "/__wake_live_reload";
const FEDERATION_CONTROL_HEADER: &str = "Wake-Federation-Control";
const FEDERATION_ACTION_HEADER: &str = "Wake-Federation-Action";
const FEDERATION_REMOTE_HEADER: &str = "Wake-Federation-Remote";
const FEDERATION_CURRENT_BUILD_HEADER: &str = "Wake-Federation-Current-Build-Id";
const FEDERATION_GENERATION_HEADER: &str = "Wake-Federation-Generation";
const FEDERATION_EXPIRED_BUILD_HEADER: &str = "Wake-Federation-Expired-Build-Id";
const FEDERATION_REASON_HEADER: &str = "Wake-Federation-Reason";
const FEDERATION_CONTROL_EXPOSE_HEADERS: &str = "Wake-Federation-Control, Wake-Federation-Action, Wake-Federation-Remote, Wake-Federation-Current-Build-Id, Wake-Federation-Generation, Wake-Federation-Expired-Build-Id, Wake-Federation-Reason";
const FEDERATION_LEASE_FRAME_MAX_BYTES: usize = 16 * 1024;

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

#[derive(Clone)]
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
#[derive(Clone, Default)]
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
    /// 非入口 chunk 的 map：`<chunk file>.map → Source Map V3 JSON`。
    chunk_maps: std::collections::HashMap<String, String>,
    /// 若最近一次构建失败，格式化后的诊断文本；否则 `None`。
    error: Option<String>,
}

/// One externally observable mount generation. Bundle bytes and Federation routes must move
/// together: the bootstrap in one snapshot binds the unscoped development bundle to that
/// snapshot's build ID, so publishing them through separate locks can expose a crossed generation.
#[derive(Default)]
struct PublishedMountGeneration {
    bundle: BundleState,
    html: String,
    federation: federation::FederationSnapshotState,
}

struct PublishedMountCandidate {
    bundle: BundleState,
    html: String,
    federation: Option<(federation::FederationSnapshot, Option<DevUpdate>)>,
}

impl PublishedMountGeneration {
    fn install(&mut self, candidate: PublishedMountCandidate) -> Option<DevUpdate> {
        let PublishedMountCandidate {
            bundle,
            html,
            federation,
        } = candidate;
        self.bundle = bundle;
        self.html = html;
        federation.and_then(|(snapshot, update)| {
            self.federation.install(snapshot);
            update
        })
    }
}

/// HTTP 处理器共享数据。
struct AppState {
    mounts: Arc<Vec<Arc<MountedAppState>>>,
    /// Browser update broadcast. Ordinary frames are encoded from [`LiveReloadMessage`];
    /// Federation isolated-remount frames use their separately versioned contract.
    tx: broadcast::Sender<String>,
    /// 代理规则（已编译）；命中前缀的请求转发到后端 target。
    proxies: Arc<Vec<CompiledProxy>>,
    stop: Arc<StopSignal>,
}

struct MountedAppState {
    name: Option<String>,
    base_path: String,
    published: Arc<RwLock<PublishedMountGeneration>>,
    public_dir: PathBuf,
    /// Remote-local protocol transport. A noisy sibling remote cannot lag or refresh this mount's
    /// subscribers because each enabled container owns a distinct bounded channel.
    federation_tx: broadcast::Sender<String>,
    loading: Arc<MountLoadingState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MountLoadPhase {
    Pending,
    Queued(u64),
    Building(u64),
    Loaded,
    Failed(String),
    Stopped(String),
}

#[derive(Debug)]
struct MountLoadState {
    phase: MountLoadPhase,
    next_epoch: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MountLoadTicket {
    index: usize,
    epoch: u64,
}

struct MountLoadingState {
    state: Mutex<MountLoadState>,
    changed: watch::Sender<()>,
    load_tx: mpsc::Sender<MountLoadTicket>,
    index: usize,
}

#[derive(Debug)]
enum MountIdlePhase {
    Pending,
    Loaded,
    Failed(String),
}

#[derive(Debug)]
enum MountAttemptCompletion {
    Retryable,
    Loaded,
    Failed(String),
    Stopped(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum MountReadiness {
    Ready,
    Failed(String),
    Wait,
    Enqueue(MountLoadTicket),
}

#[derive(Debug)]
struct StopSignal {
    requested: AtomicBool,
    changed: watch::Sender<bool>,
}

impl StopSignal {
    fn new() -> Self {
        let (changed, _) = watch::channel(false);
        Self {
            requested: AtomicBool::new(false),
            changed,
        }
    }

    fn request(&self) {
        if !self.requested.swap(true, Ordering::AcqRel) {
            self.changed.send_replace(true);
        }
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.changed.subscribe()
    }
}

impl MountLoadingState {
    fn new(index: usize, phase: MountLoadPhase, load_tx: mpsc::Sender<MountLoadTicket>) -> Self {
        let (changed, _) = watch::channel(());
        Self {
            state: Mutex::new(MountLoadState {
                phase,
                next_epoch: 1,
            }),
            changed,
            load_tx,
            index,
        }
    }

    fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    fn phase(&self) -> MountLoadPhase {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .phase
            .clone()
    }

    fn notify_changed(&self) {
        self.changed.send_replace(());
    }

    fn poll_readiness(&self) -> MountReadiness {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match &state.phase {
            MountLoadPhase::Loaded => MountReadiness::Ready,
            MountLoadPhase::Failed(error) | MountLoadPhase::Stopped(error) => {
                MountReadiness::Failed(error.clone())
            }
            MountLoadPhase::Queued(_) | MountLoadPhase::Building(_) => MountReadiness::Wait,
            MountLoadPhase::Pending => {
                let epoch = state.next_epoch;
                let Some(next_epoch) = state.next_epoch.checked_add(1) else {
                    let error = "Wake workspace loader exhausted its attempt epochs".to_owned();
                    state.phase = MountLoadPhase::Failed(error.clone());
                    self.notify_changed();
                    return MountReadiness::Failed(error);
                };
                state.next_epoch = next_epoch;
                state.phase = MountLoadPhase::Queued(epoch);
                self.notify_changed();
                MountReadiness::Enqueue(MountLoadTicket {
                    index: self.index,
                    epoch,
                })
            }
        }
    }

    fn enqueue(&self, ticket: MountLoadTicket) {
        if self.load_tx.send(ticket).is_err() {
            self.complete_queued_stopped(ticket.epoch, "Wake workspace loader stopped".to_owned());
        }
    }

    fn complete_queued_stopped(&self, epoch: u64, error: String) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.phase != MountLoadPhase::Queued(epoch) {
            return false;
        }
        state.phase = MountLoadPhase::Stopped(error);
        self.notify_changed();
        true
    }

    fn claim(&self, ticket: MountLoadTicket) -> bool {
        if ticket.index != self.index {
            return false;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.phase != MountLoadPhase::Queued(ticket.epoch) {
            return false;
        }
        state.phase = MountLoadPhase::Building(ticket.epoch);
        self.notify_changed();
        true
    }

    fn complete_attempt(&self, epoch: u64, completion: MountAttemptCompletion) -> bool {
        let phase = match completion {
            MountAttemptCompletion::Retryable => MountLoadPhase::Pending,
            MountAttemptCompletion::Loaded => MountLoadPhase::Loaded,
            MountAttemptCompletion::Failed(error) => MountLoadPhase::Failed(error),
            MountAttemptCompletion::Stopped(error) => MountLoadPhase::Stopped(error),
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.phase != MountLoadPhase::Building(epoch) {
            return false;
        }
        state.phase = phase;
        self.notify_changed();
        true
    }

    fn recover_backend_loss(&self) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !matches!(state.phase, MountLoadPhase::Building(_)) {
            return false;
        }
        state.phase = MountLoadPhase::Pending;
        self.notify_changed();
        true
    }

    fn set_idle_phase(&self, phase: MountIdlePhase) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let owns_lazy_attempt = match state.phase {
            MountLoadPhase::Queued(_) => true,
            MountLoadPhase::Building(epoch) => epoch != 0,
            _ => false,
        };
        if owns_lazy_attempt || matches!(state.phase, MountLoadPhase::Stopped(_)) {
            return false;
        }
        state.phase = match phase {
            MountIdlePhase::Pending => MountLoadPhase::Pending,
            MountIdlePhase::Loaded => MountLoadPhase::Loaded,
            MountIdlePhase::Failed(error) => MountLoadPhase::Failed(error),
        };
        self.notify_changed();
        true
    }

    fn stop(&self, error: String) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if matches!(
            state.phase,
            MountLoadPhase::Pending | MountLoadPhase::Queued(_) | MountLoadPhase::Building(_)
        ) {
            state.phase = MountLoadPhase::Stopped(error);
            self.notify_changed();
        }
    }
}

struct MountWaiterFinalizer {
    loading: Vec<Arc<MountLoadingState>>,
}

impl MountWaiterFinalizer {
    fn new(mounts: &[Arc<MountedAppState>]) -> Self {
        Self {
            loading: mounts
                .iter()
                .map(|mount| Arc::clone(&mount.loading))
                .collect(),
        }
    }
}

impl Drop for MountWaiterFinalizer {
    fn drop(&mut self) {
        for loading in &self.loading {
            loading
                .stop("Wake development worker stopped before the mount became ready".to_owned());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevLoading {
    Lazy,
    Eager,
}

/// A mount-owned filesystem interest.
///
/// Trees apply Wake's source-file filter. Exact files are control inputs and therefore bypass
/// extension filtering (`.browserslistrc` and `wake-federation.lock` intentionally have no useful
/// source extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WatchTreeFilter {
    SourceFiles,
    AllFiles,
}

/// Why a retained development build must refresh.
///
/// `Rescan` is deliberately distinct from an empty path batch: it means watcher coverage may
/// have been absent and every filesystem-backed cache plus authoritative configuration must be
/// reread. Callers must never silently downgrade it to `Paths(Vec::new())`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchInvalidation {
    Paths(Vec<PathBuf>),
    Rescan,
}

impl WatchInvalidation {
    pub fn paths(&self) -> &[PathBuf] {
        match self {
            Self::Paths(paths) => paths,
            Self::Rescan => &[],
        }
    }

    pub fn is_rescan(&self) -> bool {
        matches!(self, Self::Rescan)
    }

    /// Resolve filesystem events to the same physical identity used by watch interests and
    /// compiler caches. Missing paths retain their suffix below the nearest existing ancestor.
    pub fn normalized(self) -> Self {
        match self {
            Self::Paths(paths) => Self::Paths(
                paths
                    .into_iter()
                    .map(|path| normalize_watch_path(&path))
                    .collect(),
            ),
            Self::Rescan => Self::Rescan,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum WatchInterest {
    Tree {
        declared: PathBuf,
        resolved: PathBuf,
        filter: WatchTreeFilter,
        /// Declared and currently resolved trees which are owned outputs rather than inputs.
        excluded: Vec<(PathBuf, PathBuf)>,
        watch_declared_parent: bool,
    },
    ExactFile {
        declared: PathBuf,
        resolved: PathBuf,
    },
}

impl WatchInterest {
    pub fn tree(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self::Tree {
            declared: path.clone(),
            resolved: path,
            filter: WatchTreeFilter::SourceFiles,
            excluded: Vec::new(),
            watch_declared_parent: false,
        }
    }

    pub fn all_files_tree(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self::Tree {
            declared: path.clone(),
            resolved: path,
            filter: WatchTreeFilter::AllFiles,
            excluded: Vec::new(),
            watch_declared_parent: false,
        }
    }

    pub fn exact_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self::ExactFile {
            declared: path.clone(),
            resolved: path,
        }
    }

    /// Exclude an owned output tree from this input interest. The exclusion retains both its
    /// lexical and resolved identity so symlinked output paths cannot feed a watch build.
    pub fn excluding_tree(mut self, path: impl Into<PathBuf>) -> Self {
        if let Self::Tree { excluded, .. } = &mut self {
            let path = path.into();
            excluded.push((path.clone(), path));
            excluded.sort();
            excluded.dedup();
        }
        self
    }

    /// Resolve a declared interest against its owning root while preserving both its lexical and
    /// current canonical identity. Frontends which register watchers directly must call this
    /// before consuming [`Self::registrations`].
    pub fn resolve_against(&self, root: &Path) -> Self {
        match self {
            Self::Tree {
                declared,
                filter,
                excluded,
                ..
            } => {
                let declared = resolve_declared_watch_path(root, declared);
                let mut excluded = excluded
                    .iter()
                    .map(|(declared, _)| {
                        let declared = resolve_declared_watch_path(root, declared);
                        let resolved = normalize_watch_path(&declared);
                        (declared, resolved)
                    })
                    .collect::<Vec<_>>();
                excluded.sort();
                excluded.dedup();
                Self::Tree {
                    resolved: normalize_watch_path(&declared),
                    watch_declared_parent: watch_path_is_link_or_reparse(&declared),
                    declared,
                    filter: *filter,
                    excluded,
                }
            }
            Self::ExactFile { declared, .. } => {
                let declared = resolve_declared_watch_path(root, declared);
                Self::ExactFile {
                    resolved: normalize_watch_path(&declared),
                    declared,
                }
            }
        }
    }

    pub fn matches(&self, path: &Path) -> bool {
        let declared_event = wake_common::fs::normalize(path);
        let resolved_event = normalize_watch_path(path);
        match self {
            Self::Tree {
                declared,
                resolved,
                filter,
                excluded,
                ..
            } => {
                if watch_path_is_excluded(&declared_event, &resolved_event, excluded) {
                    return false;
                }
                let declared_in_tree = path_starts_with(&declared_event, declared)
                    && !has_internal_generated_component(&declared_event, declared, *filter);
                let resolved_in_tree = path_starts_with(&resolved_event, resolved)
                    && !has_internal_generated_component(&resolved_event, resolved, *filter);
                let in_tree = declared_in_tree || resolved_in_tree;
                in_tree
                    && (paths_equal(&declared_event, declared)
                        || paths_equal(&resolved_event, resolved)
                        || *filter == WatchTreeFilter::AllFiles
                        || declared_event
                            .extension()
                            .and_then(|extension| extension.to_str())
                            .is_some_and(is_watched_ext))
            }
            Self::ExactFile { declared, resolved } => {
                paths_equal(&declared_event, declared) || paths_equal(&resolved_event, resolved)
            }
        }
    }

    pub fn matches_exact_file(&self, path: &Path) -> bool {
        matches!(self, Self::ExactFile { .. }) && self.matches(path)
    }

    pub fn matches_event(&self, path: &Path, structural: bool) -> bool {
        if self.matches(path) {
            return true;
        }
        if !structural {
            return false;
        }
        let declared_event = wake_common::fs::normalize(path);
        let resolved_event = normalize_watch_path(path);
        match self {
            Self::Tree {
                declared,
                resolved,
                filter,
                excluded,
                ..
            } => {
                if watch_path_is_excluded(&declared_event, &resolved_event, excluded) {
                    return false;
                }
                ((path_starts_with(&declared_event, declared)
                    && !has_internal_generated_component(&declared_event, declared, *filter))
                    || (path_starts_with(&resolved_event, resolved)
                        && !has_internal_generated_component(&resolved_event, resolved, *filter)))
                    || path_starts_with(declared, &declared_event)
                    || path_starts_with(resolved, &resolved_event)
            }
            Self::ExactFile { declared, resolved } => {
                path_starts_with(declared, &declared_event)
                    || path_starts_with(resolved, &resolved_event)
            }
        }
    }

    pub fn registrations(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let (paths, tree) = match self {
            Self::Tree {
                declared, resolved, ..
            } => ([declared, resolved], true),
            Self::ExactFile { declared, resolved } => ([declared, resolved], false),
        };
        let mut registrations = paths
            .into_iter()
            .filter_map(|path| {
                if tree && path.is_dir() {
                    Some((path.clone(), RecursiveMode::Recursive))
                } else {
                    nearest_existing_watch_ancestor(path)
                        .map(|ancestor| (ancestor, RecursiveMode::NonRecursive))
                }
            })
            .collect::<Vec<_>>();
        if let Self::Tree {
            declared,
            watch_declared_parent: true,
            ..
        } = self
            && let Some(parent) = declared.parent().and_then(nearest_existing_watch_ancestor)
        {
            registrations.push((parent, RecursiveMode::NonRecursive));
        }
        registrations.sort_by(|left, right| left.0.cmp(&right.0));
        registrations.dedup();
        registrations
    }

    pub fn registration(&self) -> Option<(PathBuf, RecursiveMode)> {
        self.registrations().into_iter().next()
    }
}

fn watch_path_is_link_or_reparse(path: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn watch_path_is_excluded(
    declared_event: &Path,
    resolved_event: &Path,
    excluded: &[(PathBuf, PathBuf)],
) -> bool {
    excluded.iter().any(|(declared, resolved)| {
        path_starts_with(declared_event, declared) || path_starts_with(resolved_event, resolved)
    })
}

fn has_internal_generated_component(path: &Path, root: &Path, filter: WatchTreeFilter) -> bool {
    if !path_starts_with(path, root) {
        return false;
    }
    path.components()
        .skip(root.components().count())
        .any(|component| {
            let name = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            (filter == WatchTreeFilter::SourceFiles && name == ".wake")
                || [
                    ".wake-output-stage-",
                    ".wake-app-backup-",
                    ".wake-docs-backup-",
                    ".wake-docs-next-",
                    ".wake-docs-previous-",
                    ".wake-exact-stage-",
                    ".wake-exact-backup-",
                    ".wake-bundle-",
                    ".wake-generated-next-",
                    ".wake-generated-previous-",
                ]
                .iter()
                .any(|prefix| name.starts_with(prefix))
        })
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn path_starts_with(path: &Path, base: &Path) -> bool {
    #[cfg(windows)]
    {
        let mut path = path.components();
        for expected in base.components() {
            let Some(actual) = path.next() else {
                return false;
            };
            if !actual
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&expected.as_os_str().to_string_lossy())
            {
                return false;
            }
        }
        true
    }
    #[cfg(not(windows))]
    {
        path.starts_with(base)
    }
}

fn resolve_declared_watch_path(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        wake_common::fs::normalize(path)
    } else {
        wake_common::fs::normalize(&root.join(path))
    }
}

fn normalize_watch_path(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return wake_common::fs::normalize(&path);
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(file_name) = ancestor.file_name() else {
            return wake_common::fs::normalize(path);
        };
        suffix.push(file_name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return wake_common::fs::normalize(path);
        };
        ancestor = parent;
    }
    let mut normalized = ancestor
        .canonicalize()
        .map(|path| wake_common::fs::normalize(&path))
        .unwrap_or_else(|_| wake_common::fs::normalize(ancestor));
    for component in suffix.iter().rev() {
        normalized.push(component);
    }
    normalized
}

fn reported_watch_paths(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut reported = Vec::<PathBuf>::with_capacity(paths.len());
    for path in paths {
        let identity = normalize_watch_path(path);
        if !reported
            .iter()
            .any(|existing| paths_equal(existing, &identity))
        {
            reported.push(identity);
        }
    }
    reported.sort();
    reported
}

fn nearest_existing_watch_ancestor(path: &Path) -> Option<PathBuf> {
    let mut candidate = path;
    loop {
        if candidate.is_dir() {
            return Some(normalize_watch_path(candidate));
        }
        candidate = candidate.parent()?;
    }
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
    /// Filesystem view for generated compile inputs. Application orchestrators may project an
    /// isolated physical generation onto stable logical module paths.
    pub file_system: Option<Arc<dyn FileSystem>>,
    pub watch_interests: Vec<WatchInterest>,
    pub refresh: Option<RefreshMount>,
    pub federation: FederationBuildOptions,
}

/// A lazy mount whose compile plan does not exist yet. Only authoritative preliminary interests
/// and immutable routing topology are present before watcher registration; the refresh callback
/// may materialize a candidate after the registration Rescan or first request.
pub struct DeferredMountedServeOptions {
    pub name: String,
    pub root: PathBuf,
    pub base_path: String,
    pub watch_interests: Vec<WatchInterest>,
    pub refresh: DeferredRefreshMount,
    pub federation: FederationBuildOptions,
}

/// Compile inputs that may be replaced atomically while a server mount remains live. Server
/// topology, URL ownership, public files and Federation transport stay on [`MountSpec`] and require
/// a restart when they change.
#[derive(Clone)]
pub struct DevMountPlan {
    pub entry: PathBuf,
    pub resolve_options: ResolveOptions,
    pub define: Vec<(String, String)>,
    pub target_env: TargetEnv,
    pub jsx_import_source: String,
    pub file_system: Arc<dyn FileSystem>,
}

impl std::fmt::Debug for DevMountPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DevMountPlan")
            .field("entry", &self.entry)
            .field("resolve_options", &self.resolve_options)
            .field("define", &self.define)
            .field("target_env", &self.target_env)
            .field("jsx_import_source", &self.jsx_import_source)
            .field("file_system", &"<mount-owned>")
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefreshOutcome {
    Committed,
    RetryableFailure,
    Superseded,
    Aborted,
}

pub struct DevMountMaterialization {
    pub plan: DevMountPlan,
    pub watch_interests: Vec<WatchInterest>,
    pub generated_paths: Vec<PathBuf>,
}

pub type RefreshCompletion = Box<dyn FnOnce(RefreshOutcome) + Send + 'static>;
type MaterializeMount =
    Box<dyn FnOnce() -> Result<DevMountMaterialization, Diagnostic> + Send + 'static>;

/// A move-only refresh candidate. Its preliminary interests are available before any generated
/// input is materialized. Dropping an unfinished candidate is an explicit abort rather than a
/// silent lost completion.
pub struct DevMountCandidate {
    watch_interests: Vec<WatchInterest>,
    materialize: Option<MaterializeMount>,
    completion: Option<RefreshCompletion>,
}

impl DevMountCandidate {
    pub fn new(
        watch_interests: Vec<WatchInterest>,
        materialize: impl FnOnce() -> Result<DevMountMaterialization, Diagnostic> + Send + 'static,
        completion: impl FnOnce(RefreshOutcome) + Send + 'static,
    ) -> Self {
        Self {
            watch_interests,
            materialize: Some(Box::new(materialize)),
            completion: Some(Box::new(completion)),
        }
    }

    pub fn watch_interests(&self) -> &[WatchInterest] {
        &self.watch_interests
    }

    #[allow(clippy::result_large_err)]
    pub fn materialize(&mut self) -> Result<DevMountMaterialization, Diagnostic> {
        match self.materialize_unfinished() {
            Ok(materialized) => Ok(materialized),
            Err(diagnostic) => {
                self.complete(RefreshOutcome::RetryableFailure);
                Err(diagnostic)
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn materialize_unfinished(&mut self) -> Result<DevMountMaterialization, Diagnostic> {
        let Some(materialize) = self.materialize.take() else {
            return Err(Diagnostic::error(
                "development refresh candidate was already materialized",
            )
            .with_code("WAKE_INTERNAL"));
        };
        materialize()
    }

    pub fn finish(mut self, outcome: RefreshOutcome) {
        self.complete(outcome);
    }

    fn complete(&mut self, outcome: RefreshOutcome) {
        if let Some(completion) = self.completion.take() {
            completion(outcome);
        }
    }
}

impl Drop for DevMountCandidate {
    fn drop(&mut self) {
        self.complete(RefreshOutcome::Aborted);
    }
}

pub enum DevMountRefresh {
    Invalidate {
        generated_paths: Vec<PathBuf>,
    },
    Candidate(DevMountCandidate),
    RejectedCandidate {
        watch_interests: Vec<WatchInterest>,
        diagnostic: Diagnostic,
    },
    RestartRequired {
        reason: String,
    },
}

/// Re-derive a mount plan from its authoritative configuration source before invalidating the
/// current session. The server commits a replacement plan/session only after its candidate build
/// succeeds.
pub type RefreshMount = Arc<
    dyn Fn(&DevMountPlan, &WatchInvalidation) -> Result<DevMountRefresh, Diagnostic>
        + Send
        + Sync
        + 'static,
>;

/// Refresh policy for a mount that has no accepted compile plan yet.
pub type DeferredRefreshMount =
    Arc<dyn Fn(&WatchInvalidation) -> Result<DevMountRefresh, Diagnostic> + Send + Sync + 'static>;
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
        sources: Vec<DiagnosticSource>,
    },
    WorkspaceState {
        total: usize,
        loaded: usize,
        failed: usize,
        current: Option<String>,
        failed_names: Vec<String>,
    },
    FederationUpdated {
        remote: String,
        old_build_id: Option<String>,
        new_build_id: String,
        changed_exposes: Vec<String>,
        types_hash: Option<String>,
        action: DevUpdateAction,
    },
    Closed,
}

/// Source text captured from the same generation-scoped filesystem view that produced a build
/// diagnostic. Event consumers must use these bytes instead of reopening a mutable host path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticSource {
    pub path: PathBuf,
    pub text: String,
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
    /// Filesystem view for generated compile inputs. `None` uses the host filesystem directly.
    pub file_system: Option<Arc<dyn FileSystem>>,
    /// Additional mount-owned interests. The default source tree, entry and public tree are always
    /// retained; these interests are additive.
    pub watch_interests: Vec<WatchInterest>,
    /// Application-owned refresh policy for generated inputs and configuration changes.
    pub refresh: Option<RefreshMount>,
    /// Suppress terminal presentation; library frontends should enable this.
    pub quiet: bool,
    /// Optional structured event sink used by library frontends.
    pub event_handler: Option<EventHandler>,
    /// Additional independently bundled applications mounted below this server.
    pub mounts: Vec<MountedServeOptions>,
    /// Lazy mounts whose generated inputs and compile plans are intentionally absent at startup.
    pub deferred_mounts: Vec<DeferredMountedServeOptions>,
    /// Federation linker controls; disabled by default for ordinary development builds.
    pub federation: FederationBuildOptions,
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
            file_system: None,
            watch_interests: Vec::new(),
            refresh: None,
            quiet: false,
            event_handler: None,
            mounts: Vec::new(),
            deferred_mounts: Vec::new(),
            federation: FederationBuildOptions::default(),
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

#[derive(Clone)]
struct MountSpec {
    name: Option<String>,
    root: PathBuf,
    base_path: String,
    loading: DevLoading,
    plan: Option<DevMountPlan>,
    watch_interests: Vec<WatchInterest>,
    refresh: Option<RefreshMount>,
    deferred_refresh: Option<DeferredRefreshMount>,
    federation: FederationBuildOptions,
    federation_updates_url: String,
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

fn federation_updates_url(host: &str, port: u16, container: &str) -> String {
    let host = if host == "0.0.0.0" { "localhost" } else { host };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("ws://{host}:{port}/__wake_federation_updates?remote={container}")
}

fn run_server(
    root: &Path,
    port: u16,
    options: ServeOptions,
    started_tx: mpsc::Sender<Result<StartedServer, String>>,
    stop: Arc<StopSignal>,
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
        file_system,
        watch_interests,
        refresh,
        quiet,
        event_handler,
        mounts,
        deferred_mounts,
        federation,
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
    let primary_updates_url = if federation.enabled {
        federation_updates_url(&host, port, &federation.container_name)
    } else {
        String::new()
    };
    let mut specs = vec![MountSpec {
        name: None,
        root: root.clone(),
        base_path: base_path.clone(),
        loading: DevLoading::Eager,
        plan: Some(DevMountPlan {
            entry,
            resolve_options,
            define,
            target_env,
            jsx_import_source,
            file_system: file_system.unwrap_or_else(|| Arc::new(OsFileSystem)),
        }),
        watch_interests,
        refresh,
        deferred_refresh: None,
        federation,
        federation_updates_url: primary_updates_url,
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
        let mount_updates_url = if mount.federation.enabled {
            federation_updates_url(&host, port, &mount.federation.container_name)
        } else {
            String::new()
        };
        specs.push(MountSpec {
            name: Some(mount.name),
            root: mount_root,
            base_path: mount_base,
            loading: mount.loading,
            plan: Some(DevMountPlan {
                entry: mount_entry,
                resolve_options: mount.resolve_options,
                define: mount.define,
                target_env: mount.target_env,
                jsx_import_source: mount.jsx_import_source,
                file_system: mount.file_system.unwrap_or_else(|| Arc::new(OsFileSystem)),
            }),
            watch_interests: mount.watch_interests,
            refresh: mount.refresh,
            deferred_refresh: None,
            federation: mount.federation,
            federation_updates_url: mount_updates_url,
        });
    }
    for mount in deferred_mounts {
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
        let mount_updates_url = if mount.federation.enabled {
            federation_updates_url(&host, port, &mount.federation.container_name)
        } else {
            String::new()
        };
        specs.push(MountSpec {
            name: Some(mount.name),
            root: mount_root,
            base_path: mount_base,
            loading: DevLoading::Lazy,
            plan: None,
            watch_interests: mount.watch_interests,
            refresh: None,
            deferred_refresh: Some(mount.refresh),
            federation: mount.federation,
            federation_updates_url: mount_updates_url,
        });
    }
    for spec in &specs {
        let Some(plan) = spec.plan.as_ref() else {
            continue;
        };
        if !plan.file_system.is_file(&plan.entry) {
            return Err(std::io::Error::other(format!(
                "entry file does not exist for Wake dev mount `{}`: {}",
                spec.name.as_deref().unwrap_or("site"),
                plan.entry.display()
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
    let mut federation_owners = BTreeMap::<String, String>::new();
    for spec in specs.iter().filter(|spec| spec.federation.enabled) {
        let owner = spec.name.as_deref().unwrap_or("site").to_owned();
        if let Some(previous) =
            federation_owners.insert(spec.federation.container_name.clone(), owner.clone())
        {
            return Err(std::io::Error::other(format!(
                "duplicate enabled Federation container name `{}` for Wake dev mounts `{previous}` and `{owner}`",
                spec.federation.container_name
            )));
        }
    }

    let sty = Sty::detect(quiet);
    let (tx, _rx) = broadcast::channel::<String>(64);
    let (load_tx, load_rx) = mpsc::channel::<MountLoadTicket>();
    let mounted_states = Arc::new(
        specs
            .iter()
            .enumerate()
            .map(|(index, spec)| {
                let (federation_tx, _federation_rx) = broadcast::channel::<String>(64);
                Arc::new(MountedAppState {
                    name: spec.name.clone(),
                    base_path: spec.base_path.clone(),
                    published: Arc::new(RwLock::new(PublishedMountGeneration {
                        html: load_html_template(
                            &spec.root,
                            &spec.base_path,
                            spec.name.as_deref(),
                            spec.federation.enabled && spec.federation.bootstrap.is_some(),
                        ),
                        ..PublishedMountGeneration::default()
                    })),
                    public_dir: spec.root.join("public"),
                    federation_tx,
                    loading: Arc::new(MountLoadingState::new(
                        index,
                        if spec.loading == DevLoading::Lazy {
                            MountLoadPhase::Pending
                        } else {
                            // Eager mounts use epoch zero; no queue ticket owns their startup.
                            MountLoadPhase::Building(0)
                        },
                        load_tx.clone(),
                    )),
                })
            })
            .collect::<Vec<_>>(),
    );
    let federation_senders = specs
        .iter()
        .zip(mounted_states.iter())
        .filter(|(spec, _)| spec.federation.enabled)
        .map(|(spec, mount)| {
            (
                spec.federation.container_name.clone(),
                mount.federation_tx.clone(),
            )
        })
        .collect::<BTreeMap<_, _>>();

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
            stop.request();
            let _ = watcher_join.join();
            return Err(std::io::Error::other(error));
        }
        Err(error) => {
            stop.request();
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
                sty.dim("Live reload on  ·  source maps on  ·  watching for changes")
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
        tx: tx.clone(),
        proxies: Arc::new(proxies),
        stop: Arc::clone(&stop),
    });
    let server = HttpServer::new(move || {
        App::new()
            .app_data(data.clone())
            // 放宽负载上限，便于代理转发较大的 POST 请求体。
            .app_data(web::PayloadConfig::new(64 * 1024 * 1024))
            .route("/__wake/client.js", web::get().to(serve_client))
            .route(LIVE_RELOAD_ENDPOINT, web::get().to(live_reload_ws_handler))
            .route(
                "/__wake_federation_updates",
                web::get().to(federation_ws_handler),
            )
            // 默认服务：先试代理转发（任意方法），未命中且为 GET 则回退 SPA HTML。
            .default_service(web::to(serve_default))
    })
    .bind((host.as_str(), port));
    let server = match server {
        Ok(server) => server.workers(4).run(),
        Err(error) => {
            stop.request();
            let _ = watcher_join.join();
            return Err(error);
        }
    };
    let handle = server.handle();
    if started_tx
        .send(Ok(StartedServer {
            url: url.clone(),
            handle: handle.clone(),
            federation_senders,
            event_handler: event_handler.clone(),
        }))
        .is_err()
    {
        stop.request();
        actix_web::rt::System::new().block_on(handle.stop(false));
        let _ = watcher_join.join();
        return Err(std::io::Error::other(
            "Wake dev server startup receiver was dropped",
        ));
    }
    let result = actix_web::rt::System::new().block_on(server);
    stop.request();
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

enum WatchBackendNotification {
    Paths {
        generation: u64,
        events: Vec<(PathBuf, bool)>,
    },
    Rescan {
        generation: u64,
    },
    Error {
        generation: u64,
        message: String,
    },
}

#[derive(Debug, Default)]
struct WatchBackendLeaseState {
    revoked: AtomicBool,
    commit_gate: RwLock<()>,
    error: Mutex<Option<String>>,
}

#[derive(Clone, Debug)]
struct WatchBackendLease {
    generation: u64,
    failed_generation: Arc<AtomicU64>,
    stop: Arc<StopSignal>,
    state: Arc<WatchBackendLeaseState>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchCommitRejected {
    BackendLost,
    Stopped,
}

impl WatchBackendLease {
    fn new(generation: u64, failed_generation: Arc<AtomicU64>, stop: Arc<StopSignal>) -> Self {
        Self {
            generation,
            failed_generation,
            stop,
            state: Arc::new(WatchBackendLeaseState::default()),
        }
    }

    fn generation(&self) -> u64 {
        self.generation
    }

    fn is_revoked(&self) -> bool {
        self.failed_generation.load(Ordering::Acquire) >= self.generation
            || self.state.revoked.load(Ordering::Acquire)
    }

    fn revoke(&self) {
        // Publish revocation before waiting for an already-linearized publication. This prevents
        // a second reader from starting work while the callback waits for the commit gate.
        self.failed_generation
            .fetch_max(self.generation, Ordering::Release);
        self.state.revoked.store(true, Ordering::Release);
        let _gate = self
            .state
            .commit_gate
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }

    fn revoke_with_error(&self, message: String) {
        {
            let mut error = self
                .state
                .error
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if error.is_none() {
                *error = Some(message);
            }
        }
        self.revoke();
    }

    fn take_error(&self) -> Option<String> {
        self.state
            .error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn commit<T>(&self, commit: impl FnOnce() -> T) -> Result<T, WatchCommitRejected> {
        let _gate = self
            .state
            .commit_gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.stop.is_requested() {
            return Err(WatchCommitRejected::Stopped);
        }
        if self.is_revoked() {
            return Err(WatchCommitRejected::BackendLost);
        }
        Ok(commit())
    }

    fn finish_candidate(&self, candidate: DevMountCandidate, outcome: RefreshOutcome) {
        let _gate = self
            .state
            .commit_gate
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        candidate.finish(if self.stop.is_requested() {
            RefreshOutcome::Aborted
        } else if self.is_revoked() {
            RefreshOutcome::RetryableFailure
        } else {
            outcome
        });
    }
}

fn commit_watch_backend_publication<T>(
    lease: &WatchBackendLease,
    mut candidate: Option<DevMountCandidate>,
    candidate_outcome: RefreshOutcome,
    publish: impl FnOnce() -> T,
) -> Result<T, WatchCommitRejected> {
    match lease.commit(|| {
        let value = publish();
        if let Some(candidate) = candidate.take() {
            candidate.finish(candidate_outcome);
        }
        value
    }) {
        Ok(value) => Ok(value),
        Err(error) => {
            if let Some(candidate) = candidate.take() {
                lease.finish_candidate(
                    candidate,
                    match error {
                        WatchCommitRejected::BackendLost => RefreshOutcome::RetryableFailure,
                        WatchCommitRejected::Stopped => RefreshOutcome::Aborted,
                    },
                );
            }
            Err(error)
        }
    }
}

fn create_dev_watcher(
    sender: mpsc::Sender<WatchBackendNotification>,
    lease: WatchBackendLease,
) -> Result<notify::RecommendedWatcher, String> {
    notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        forward_dev_watch_notification(&sender, &lease, result);
    })
    .map_err(|error| format!("cannot create Wake file watcher: {error}"))
}

fn forward_dev_watch_notification(
    sender: &mpsc::Sender<WatchBackendNotification>,
    lease: &WatchBackendLease,
    result: notify::Result<notify::Event>,
) {
    match result {
        Ok(event) if event.need_rescan() => {
            // `need_rescan` means the backend cannot prove continuity. Revoke synchronously before
            // queueing so an already-queued lazy load cannot publish under the lost capability.
            lease.revoke();
            let _ = sender.send(WatchBackendNotification::Rescan {
                generation: lease.generation(),
            });
        }
        Ok(event) if is_watch_event(&event) => {
            let structural = is_structural_event(&event);
            let _ = sender.send(WatchBackendNotification::Paths {
                generation: lease.generation(),
                events: event
                    .paths
                    .into_iter()
                    .map(|path| (path, structural))
                    .collect(),
            });
        }
        Ok(_) => {}
        Err(error) => {
            let message = format!("file watcher backend error: {error}");
            lease.revoke_with_error(message.clone());
            let _ = sender.send(WatchBackendNotification::Error {
                generation: lease.generation(),
                message,
            });
        }
    }
}

fn next_watch_retry_delay(current: Duration) -> Duration {
    current.saturating_mul(2).min(Duration::from_secs(2))
}

fn is_current_watch_generation(lease: &WatchBackendLease, generation: u64) -> bool {
    lease.generation() == generation
}

fn enter_watch_backend_recovery<W>(
    watcher: &mut Option<W>,
    lease: &WatchBackendLease,
    mounts: &[Arc<MountedAppState>],
    registered: &mut WatchRegistrationState,
    recreate_watcher: &mut bool,
    coverage_lost: &mut bool,
    retry_at: &mut Option<Instant>,
    retry_delay: &mut Duration,
) {
    lease.revoke();
    for mount in mounts {
        mount.loading.recover_backend_loss();
    }
    watcher.take();
    registered.clear_after_backend_loss();
    *recreate_watcher = true;
    *coverage_lost = true;
    *retry_delay = Duration::from_millis(100);
    *retry_at = Some(Instant::now() + *retry_delay);
}

#[allow(clippy::too_many_arguments)]
fn recover_after_watch_commit_rejection<W>(
    rejection: WatchCommitRejected,
    watcher: &mut Option<W>,
    lease: &WatchBackendLease,
    mounts: &[Arc<MountedAppState>],
    registered: &mut WatchRegistrationState,
    recreate_watcher: &mut bool,
    coverage_lost: &mut bool,
    retry_at: &mut Option<Instant>,
    retry_delay: &mut Duration,
    last_error: &mut Option<String>,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) -> bool {
    match rejection {
        WatchCommitRejected::BackendLost => {
            report_pending_watch_backend_error(lease, last_error, tx, event_handler);
            enter_watch_backend_recovery(
                watcher,
                lease,
                mounts,
                registered,
                recreate_watcher,
                coverage_lost,
                retry_at,
                retry_delay,
            );
            false
        }
        WatchCommitRejected::Stopped => true,
    }
}

fn next_mount_load(
    load_rx: &mpsc::Receiver<MountLoadTicket>,
    recovery_rescan: bool,
) -> Option<MountLoadTicket> {
    if recovery_rescan {
        None
    } else {
        load_rx.try_recv().ok()
    }
}

fn is_recovering_eager_mount(recovery_batch: bool, loading: DevLoading) -> bool {
    recovery_batch && loading == DevLoading::Eager
}

fn schedule_watch_coverage_retry(
    message: String,
    retry_at: &mut Option<Instant>,
    retry_delay: &mut Duration,
    coverage_lost: &mut bool,
    last_error: &mut Option<String>,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) {
    if last_error.as_deref() != Some(message.as_str()) {
        report_watch_failure(message.clone(), tx, event_handler);
        *last_error = Some(message);
    }
    *coverage_lost = true;
    *retry_at = Some(Instant::now() + *retry_delay);
    *retry_delay = next_watch_retry_delay(*retry_delay);
}

#[allow(clippy::too_many_arguments)]
fn reconcile_runtime_watch_targets<W: Watcher>(
    watcher: &mut W,
    registrations: &mut WatchRegistrationState,
    interests: &[WatchInterest],
    retry_at: &mut Option<Instant>,
    retry_delay: &mut Duration,
    last_error: &mut Option<String>,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) -> Result<(), String> {
    let outcome = reconcile_watch_interests(watcher, registrations, interests)
        .map_err(|error| error.to_string())?;
    if !outcome.cleanup_errors.is_empty() {
        let cleanup_error = outcome.cleanup_errors.join("; ");
        for message in outcome.cleanup_errors {
            if last_error.as_deref() != Some(message.as_str()) {
                report_watch_failure(message.clone(), tx, event_handler);
                *last_error = Some(message);
            }
        }
        *retry_at = Some(Instant::now() + *retry_delay);
        *retry_delay = next_watch_retry_delay(*retry_delay);
        return Err(cleanup_error);
    }
    Ok(())
}

fn watch_and_rebuild(
    specs: Vec<MountSpec>,
    mounts: Arc<Vec<Arc<MountedAppState>>>,
    tx: broadcast::Sender<String>,
    ready_tx: mpsc::Sender<Result<Option<BuildSummary>, String>>,
    sty: Sty,
    load_rx: mpsc::Receiver<MountLoadTicket>,
    stop: Arc<StopSignal>,
    event_handler: Option<EventHandler>,
) {
    struct Worker {
        spec: MountSpec,
        session: Option<MountBuildSession>,
        pending_candidate: Option<DevMountCandidate>,
        /// Effective accepted + every uncommitted candidate coverage. This may deliberately
        /// over-watch after a failed candidate and only shrinks after a successful commit.
        watch_interests: Vec<WatchInterest>,
        lazy_retry_on_source: bool,
    }

    let _mount_waiter_finalizer = MountWaiterFinalizer::new(&mounts);

    let mut workers = specs
        .into_iter()
        .map(|spec| {
            let watch_interests = mount_watch_interests(&spec);
            Worker {
                spec,
                session: None,
                pending_candidate: None,
                watch_interests,
                lazy_retry_on_source: false,
            }
        })
        .collect::<Vec<_>>();
    let (evt_tx, evt_rx) = mpsc::channel::<WatchBackendNotification>();
    let failed_watch_generation = Arc::new(AtomicU64::new(0));
    let mut next_watch_generation = 2_u64;
    let mut watcher_lease =
        WatchBackendLease::new(1, Arc::clone(&failed_watch_generation), Arc::clone(&stop));
    let watcher = match create_dev_watcher(evt_tx.clone(), watcher_lease.clone()) {
        Ok(watcher) => watcher,
        Err(message) => {
            let _ = ready_tx.send(Err(message));
            return;
        }
    };
    let mut watcher = Some(watcher);
    let mut registered = WatchRegistrationState::default();
    let desired_interests = workers
        .iter()
        .flat_map(|worker| worker.watch_interests.iter().cloned())
        .collect::<Vec<_>>();
    if let Err(message) = reconcile_watch_targets(
        watcher.as_mut().expect("created watcher"),
        &mut registered,
        &desired_interests,
    ) {
        let _ = ready_tx.send(Err(message));
        return;
    }
    let mut watch_retry_at = None;
    let mut watch_retry_delay = Duration::from_millis(100);
    let mut recreate_watcher = false;
    let mut recovery_rescan = true;
    let mut watch_coverage_lost = false;
    let mut last_watch_error = None;

    let mut primary_summary = None;
    for index in 0..workers.len() {
        if workers[index].spec.loading == DevLoading::Lazy {
            continue;
        }
        let spec = workers[index].spec.clone();
        let session = create_mount_session(&spec);
        let outcome = rebuild_mount(
            session,
            &spec,
            &mounts[index],
            &tx,
            &mounts[index].federation_tx,
            &watcher_lease,
            None,
            true,
            sty,
            event_handler.as_ref(),
            |commit| match commit {
                MountRebuildCommit::Published { session, .. } => {
                    workers[index].session = Some(session);
                    set_mount_idle_phase(&mounts[index], MountIdlePhase::Loaded);
                }
                MountRebuildCommit::BuildFailed { session, error } => {
                    workers[index].session = Some(session);
                    if index == 0 {
                        set_mount_idle_phase(&mounts[index], MountIdlePhase::Loaded);
                    } else {
                        set_mount_idle_phase(&mounts[index], MountIdlePhase::Failed(error));
                    }
                }
            },
        );
        match outcome {
            MountRebuildOutcome::Published(summary) => {
                if index == 0 {
                    primary_summary = Some(summary);
                }
            }
            MountRebuildOutcome::BuildFailed => {}
            MountRebuildOutcome::BackendLost(session) => {
                workers[index].session = Some(session);
                set_mount_idle_phase(&mounts[index], MountIdlePhase::Pending);
                report_pending_watch_backend_error(
                    &watcher_lease,
                    &mut last_watch_error,
                    &tx,
                    event_handler.as_ref(),
                );
                enter_watch_backend_recovery(
                    &mut watcher,
                    &watcher_lease,
                    &mounts,
                    &mut registered,
                    &mut recreate_watcher,
                    &mut watch_coverage_lost,
                    &mut watch_retry_at,
                    &mut watch_retry_delay,
                );
                break;
            }
            MountRebuildOutcome::Stopped(session) => {
                workers[index].session = Some(session);
                return;
            }
        }
    }
    emit_workspace_state(&mounts, None, event_handler.as_ref());
    // Keep `start` behind the first authoritative Rescan. Registration closes future gaps; this
    // fence closes the create/register/initial-build window before callers observe readiness.
    let startup_primary_build_succeeded = primary_summary.is_some();
    let mut startup_ready = Some(primary_summary);

    'watch: while !stop.is_requested() {
        macro_rules! commit_or_recover {
            ($result:expr) => {
                if let Err(rejection) = $result {
                    if recover_after_watch_commit_rejection(
                        rejection,
                        &mut watcher,
                        &watcher_lease,
                        &mounts,
                        &mut registered,
                        &mut recreate_watcher,
                        &mut watch_coverage_lost,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        return;
                    }
                    continue 'watch;
                }
            };
        }
        if watcher.is_some() && watcher_lease.is_revoked() {
            report_pending_watch_backend_error(
                &watcher_lease,
                &mut last_watch_error,
                &tx,
                event_handler.as_ref(),
            );
            enter_watch_backend_recovery(
                &mut watcher,
                &watcher_lease,
                &mounts,
                &mut registered,
                &mut recreate_watcher,
                &mut watch_coverage_lost,
                &mut watch_retry_at,
                &mut watch_retry_delay,
            );
            continue;
        }
        if watch_retry_at.is_some_and(|retry| Instant::now() >= retry) {
            if recreate_watcher {
                let next_lease = WatchBackendLease::new(
                    next_watch_generation,
                    Arc::clone(&failed_watch_generation),
                    Arc::clone(&stop),
                );
                next_watch_generation = next_watch_generation.saturating_add(1);
                match create_dev_watcher(evt_tx.clone(), next_lease.clone()) {
                    Ok(created) => {
                        watcher = Some(created);
                        watcher_lease = next_lease;
                        registered.clear_after_backend_loss();
                        recreate_watcher = false;
                    }
                    Err(message) => {
                        if last_watch_error.as_deref() != Some(message.as_str()) {
                            report_watch_failure(message.clone(), &tx, event_handler.as_ref());
                            last_watch_error = Some(message);
                        }
                        watch_retry_at = Some(Instant::now() + watch_retry_delay);
                        watch_retry_delay = next_watch_retry_delay(watch_retry_delay);
                        continue;
                    }
                }
            }
            let desired_interests = workers
                .iter()
                .flat_map(|worker| worker.watch_interests.iter().cloned())
                .collect::<Vec<_>>();
            let Some(active_watcher) = watcher.as_mut() else {
                recreate_watcher = true;
                watch_retry_at = Some(Instant::now() + watch_retry_delay);
                watch_retry_delay = next_watch_retry_delay(watch_retry_delay);
                continue;
            };
            match reconcile_watch_interests(active_watcher, &mut registered, &desired_interests) {
                Ok(outcome) => {
                    for message in &outcome.cleanup_errors {
                        if last_watch_error.as_deref() != Some(message) {
                            report_watch_failure(message.clone(), &tx, event_handler.as_ref());
                            last_watch_error = Some(message.clone());
                        }
                    }
                    if !outcome.cleanup_errors.is_empty() {
                        let message = outcome.cleanup_errors.join("; ");
                        watcher_lease.revoke();
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        continue;
                    }
                    if registered.is_coverage_complete(&desired_interests) && watch_coverage_lost {
                        recovery_rescan = true;
                        watch_coverage_lost = false;
                    }
                    if outcome.cleanup_errors.is_empty()
                        && registered.is_converged(&desired_interests)
                    {
                        watch_retry_at = None;
                        watch_retry_delay = Duration::from_millis(100);
                        last_watch_error = None;
                    } else {
                        watch_retry_at = Some(Instant::now() + watch_retry_delay);
                        watch_retry_delay = next_watch_retry_delay(watch_retry_delay);
                    }
                }
                Err(error) => {
                    let message = error.to_string();
                    watcher_lease.revoke();
                    watcher.take();
                    registered.clear_after_backend_loss();
                    recreate_watcher = true;
                    schedule_watch_coverage_retry(
                        message,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut watch_coverage_lost,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    );
                    continue;
                }
            }
        }
        if watcher.is_some() && watcher_lease.is_revoked() {
            report_pending_watch_backend_error(
                &watcher_lease,
                &mut last_watch_error,
                &tx,
                event_handler.as_ref(),
            );
            enter_watch_backend_recovery(
                &mut watcher,
                &watcher_lease,
                &mounts,
                &mut registered,
                &mut recreate_watcher,
                &mut watch_coverage_lost,
                &mut watch_retry_at,
                &mut watch_retry_delay,
            );
            continue;
        }
        if watcher.is_none() || watch_coverage_lost {
            std::thread::sleep(Duration::from_millis(25));
            continue;
        }
        // A recovered backend has an observation gap. Keep queued lazy loads untouched until the
        // authoritative Rescan has superseded any candidate captured before that gap; otherwise a
        // request could publish stale configuration before recovery observes it.
        while let Some(ticket) = next_mount_load(&load_rx, recovery_rescan) {
            let index = ticket.index;
            if index == 0 || index >= workers.len() {
                continue;
            }
            // Receiving a ticket does not grant authority by itself. Only the exact queued epoch
            // can be claimed; duplicates and tickets from an earlier recovery attempt are no-ops.
            if !mounts[index].loading.claim(ticket) {
                continue;
            }
            if stop.is_requested() {
                mounts[index].loading.complete_attempt(
                    ticket.epoch,
                    MountAttemptCompletion::Stopped(
                        "Wake development server is stopping".to_owned(),
                    ),
                );
                return;
            }
            if watcher_lease.is_revoked() {
                enter_watch_backend_recovery(
                    &mut watcher,
                    &watcher_lease,
                    &mounts,
                    &mut registered,
                    &mut recreate_watcher,
                    &mut watch_coverage_lost,
                    &mut watch_retry_at,
                    &mut watch_retry_delay,
                );
                continue 'watch;
            }
            if workers[index].session.is_some() {
                mounts[index]
                    .loading
                    .complete_attempt(ticket.epoch, MountAttemptCompletion::Loaded);
                continue;
            }
            emit_workspace_state(
                &mounts,
                workers[index].spec.name.clone(),
                event_handler.as_ref(),
            );
            let refresh = match workers[index].pending_candidate.take() {
                Some(candidate) => Ok(DevMountRefresh::Candidate(candidate)),
                None => refresh_mount(&workers[index].spec, &WatchInvalidation::Rescan),
            };
            let mut candidate_spec = workers[index].spec.clone();
            let mut candidate = None;
            match refresh {
                Ok(DevMountRefresh::Invalidate { .. }) => {}
                Ok(DevMountRefresh::Candidate(mut refresh_candidate)) => {
                    workers[index].watch_interests = union_watch_interests(
                        &workers[index].watch_interests,
                        refresh_candidate.watch_interests(),
                    );
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher_lease
                            .finish_candidate(refresh_candidate, RefreshOutcome::RetryableFailure);
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message.clone(),
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        mounts[index]
                            .loading
                            .complete_attempt(ticket.epoch, MountAttemptCompletion::Retryable);
                        emit_workspace_state(&mounts, None, event_handler.as_ref());
                        continue 'watch;
                    }
                    let materialized = match refresh_candidate.materialize_unfinished() {
                        Ok(materialized) => materialized,
                        Err(diagnostic) => {
                            commit_or_recover!(report_refresh_failure(
                                &watcher_lease,
                                Some(refresh_candidate),
                                &candidate_spec,
                                &mounts[index],
                                diagnostic,
                                &tx,
                                event_handler.as_ref(),
                                |error| {
                                    workers[index].lazy_retry_on_source = true;
                                    mounts[index].loading.complete_attempt(
                                        ticket.epoch,
                                        MountAttemptCompletion::Failed(error.to_owned()),
                                    );
                                    emit_workspace_state(&mounts, None, event_handler.as_ref());
                                },
                            ));
                            continue;
                        }
                    };
                    let DevMountMaterialization {
                        plan,
                        watch_interests,
                        generated_paths: _,
                    } = materialized;
                    if let Err(diagnostic) = validate_replacement_plan(&plan) {
                        commit_or_recover!(report_refresh_failure(
                            &watcher_lease,
                            Some(refresh_candidate),
                            &candidate_spec,
                            &mounts[index],
                            diagnostic,
                            &tx,
                            event_handler.as_ref(),
                            |_| {
                                mounts[index].loading.complete_attempt(
                                    ticket.epoch,
                                    MountAttemptCompletion::Failed(
                                        "development server restart required".to_owned(),
                                    ),
                                );
                                emit_workspace_state(&mounts, None, event_handler.as_ref());
                            },
                        ));
                        continue;
                    }
                    candidate_spec.plan = Some(plan);
                    candidate_spec.watch_interests = watch_interests;
                    let candidate_interests = mount_watch_interests(&candidate_spec);
                    workers[index].watch_interests = union_watch_interests(
                        &workers[index].watch_interests,
                        &candidate_interests,
                    );
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher_lease
                            .finish_candidate(refresh_candidate, RefreshOutcome::RetryableFailure);
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message.clone(),
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        mounts[index]
                            .loading
                            .complete_attempt(ticket.epoch, MountAttemptCompletion::Retryable);
                        emit_workspace_state(&mounts, None, event_handler.as_ref());
                        continue 'watch;
                    }
                    candidate = Some(refresh_candidate);
                }
                Ok(DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                }) => {
                    workers[index].watch_interests =
                        union_watch_interests(&workers[index].watch_interests, &watch_interests);
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message.clone(),
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        mounts[index]
                            .loading
                            .complete_attempt(ticket.epoch, MountAttemptCompletion::Retryable);
                        emit_workspace_state(&mounts, None, event_handler.as_ref());
                        continue 'watch;
                    }
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &candidate_spec,
                        &mounts[index],
                        diagnostic,
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            workers[index].lazy_retry_on_source = true;
                            mounts[index].loading.complete_attempt(
                                ticket.epoch,
                                MountAttemptCompletion::Failed(error.to_owned()),
                            );
                            emit_workspace_state(&mounts, None, event_handler.as_ref());
                        },
                    ));
                    continue;
                }
                Ok(DevMountRefresh::RestartRequired { reason }) => {
                    let diagnostic = restart_required_diagnostic(reason);
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &candidate_spec,
                        &mounts[index],
                        diagnostic,
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            mounts[index].loading.complete_attempt(
                                ticket.epoch,
                                MountAttemptCompletion::Failed(error.to_owned()),
                            );
                            emit_workspace_state(&mounts, None, event_handler.as_ref());
                        },
                    ));
                    continue;
                }
                Err(diagnostic) => {
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &candidate_spec,
                        &mounts[index],
                        diagnostic,
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            mounts[index].loading.complete_attempt(
                                ticket.epoch,
                                MountAttemptCompletion::Failed(error.to_owned()),
                            );
                            emit_workspace_state(&mounts, None, event_handler.as_ref());
                        },
                    ));
                    continue;
                }
            }
            let candidate_session = create_mount_session(&candidate_spec);
            let committed_spec = candidate_spec.clone();
            let committed_interests = mount_watch_interests(&candidate_spec);
            let mut build_error = None;
            let outcome = rebuild_mount(
                candidate_session,
                &candidate_spec,
                &mounts[index],
                &tx,
                &mounts[index].federation_tx,
                &watcher_lease,
                candidate,
                true,
                sty,
                event_handler.as_ref(),
                |commit| match commit {
                    MountRebuildCommit::Published { session, .. } => {
                        workers[index].watch_interests = committed_interests;
                        workers[index].spec = committed_spec;
                        workers[index].session = Some(session);
                        workers[index].lazy_retry_on_source = false;
                    }
                    MountRebuildCommit::BuildFailed { session: _, error } => {
                        workers[index].lazy_retry_on_source = true;
                        build_error = Some(error);
                    }
                },
            );
            match outcome {
                MountRebuildOutcome::Published(_) => {
                    // `rebuild_mount` has already installed the generation, swapped the worker in
                    // the commit callback, and completed the candidate. Readiness is released last.
                    mounts[index]
                        .loading
                        .complete_attempt(ticket.epoch, MountAttemptCompletion::Loaded);
                    emit_workspace_state(&mounts, None, event_handler.as_ref());
                }
                MountRebuildOutcome::BuildFailed => {
                    mounts[index].loading.complete_attempt(
                        ticket.epoch,
                        MountAttemptCompletion::Failed(
                            build_error.unwrap_or_else(|| "Wake build failed".to_owned()),
                        ),
                    );
                    emit_workspace_state(&mounts, None, event_handler.as_ref());
                }
                MountRebuildOutcome::BackendLost(_candidate_session) => {
                    workers[index].lazy_retry_on_source = true;
                    mounts[index]
                        .loading
                        .complete_attempt(ticket.epoch, MountAttemptCompletion::Retryable);
                    report_pending_watch_backend_error(
                        &watcher_lease,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    );
                    enter_watch_backend_recovery(
                        &mut watcher,
                        &watcher_lease,
                        &mounts,
                        &mut registered,
                        &mut recreate_watcher,
                        &mut watch_coverage_lost,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                    );
                    continue 'watch;
                }
                MountRebuildOutcome::Stopped(_candidate_session) => {
                    mounts[index].loading.complete_attempt(
                        ticket.epoch,
                        MountAttemptCompletion::Stopped(
                            "Wake development server is stopping".to_owned(),
                        ),
                    );
                    return;
                }
            }
        }

        let mut changed_events = Vec::new();
        let recovery_batch = recovery_rescan;
        let mut batch_rescan = std::mem::take(&mut recovery_rescan);
        let mut backend_error = None;
        let first_notification = if batch_rescan {
            None
        } else {
            match evt_rx.recv_timeout(Duration::from_millis(25)) {
                Ok(notification) => Some(notification),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        };
        {
            let mut accept_notification = |notification| match notification {
                WatchBackendNotification::Paths { generation, events }
                    if is_current_watch_generation(&watcher_lease, generation) =>
                {
                    changed_events.extend(events);
                }
                WatchBackendNotification::Rescan { generation }
                    if is_current_watch_generation(&watcher_lease, generation) =>
                {
                    batch_rescan = true;
                }
                WatchBackendNotification::Error {
                    generation,
                    message,
                } if is_current_watch_generation(&watcher_lease, generation) => {
                    backend_error = Some(message);
                }
                // A retired backend can still have messages buffered in the channel. Its entire
                // event stream is stale and must not cross the successor generation's recovery
                // fence.
                _ => {}
            };
            if let Some(notification) = first_notification {
                accept_notification(notification);
            }
            loop {
                match evt_rx.recv_timeout(WATCH_SETTLE_QUIET) {
                    Ok(notification) => accept_notification(notification),
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
        }
        if watcher_lease.is_revoked() {
            report_pending_watch_backend_error(
                &watcher_lease,
                &mut last_watch_error,
                &tx,
                event_handler.as_ref(),
            );
            enter_watch_backend_recovery(
                &mut watcher,
                &watcher_lease,
                &mounts,
                &mut registered,
                &mut recreate_watcher,
                &mut watch_coverage_lost,
                &mut watch_retry_at,
                &mut watch_retry_delay,
            );
            continue;
        }
        let completes_startup_fence = startup_ready.is_some() && batch_rescan;
        if let Some(message) = backend_error {
            let _ = watcher_lease.take_error();
            if last_watch_error.as_deref() != Some(message.as_str()) {
                report_watch_failure(message.clone(), &tx, event_handler.as_ref());
                last_watch_error = Some(message);
            }
            enter_watch_backend_recovery(
                &mut watcher,
                &watcher_lease,
                &mounts,
                &mut registered,
                &mut recreate_watcher,
                &mut watch_coverage_lost,
                &mut watch_retry_at,
                &mut watch_retry_delay,
            );
            continue;
        }
        let mut changed = BTreeMap::<PathBuf, bool>::new();
        for (path, structural) in changed_events {
            for identity in [
                wake_common::fs::normalize(&path),
                normalize_watch_path(&path),
            ] {
                changed
                    .entry(identity)
                    .and_modify(|current| *current |= structural)
                    .or_insert(structural);
            }
        }
        // A tree may not exist when the server starts (notably `public/`). Its parent is watched
        // non-recursively until the create event arrives; then promote the new tree to a recursive
        // registration before subsequent child events can be missed.
        let routing_interests = if changed.values().any(|structural| *structural) {
            workers
                .iter_mut()
                .map(|worker| {
                    let previous = worker.watch_interests.clone();
                    let current = worker
                        .watch_interests
                        .iter()
                        .map(|interest| interest.resolve_against(&worker.spec.root))
                        .collect::<Vec<_>>();
                    worker.watch_interests = current.clone();
                    union_watch_interests(&previous, &current)
                })
                .collect::<Vec<_>>()
        } else {
            workers
                .iter()
                .map(|worker| worker.watch_interests.clone())
                .collect::<Vec<_>>()
        };
        let desired_interests = workers
            .iter()
            .flat_map(|worker| worker.watch_interests.iter().cloned())
            .collect::<Vec<_>>();
        match reconcile_watch_interests(
            watcher.as_mut().expect("active watcher"),
            &mut registered,
            &desired_interests,
        ) {
            Ok(outcome) => {
                if !outcome.cleanup_errors.is_empty() {
                    let message = outcome.cleanup_errors.join("; ");
                    watcher_lease.revoke();
                    watcher.take();
                    registered.clear_after_backend_loss();
                    recreate_watcher = true;
                    schedule_watch_coverage_retry(
                        message,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut watch_coverage_lost,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    );
                    continue;
                }
            }
            Err(error) => {
                let message = error.to_string();
                watcher_lease.revoke();
                watcher.take();
                registered.clear_after_backend_loss();
                recreate_watcher = true;
                if last_watch_error.as_deref() != Some(message.as_str()) {
                    report_watch_failure(message.clone(), &tx, event_handler.as_ref());
                    last_watch_error = Some(message);
                }
                watch_coverage_lost = true;
                watch_retry_at = Some(Instant::now() + watch_retry_delay);
                watch_retry_delay = next_watch_retry_delay(watch_retry_delay);
                continue;
            }
        }
        let affected = workers
            .iter()
            .enumerate()
            .filter_map(|(index, _worker)| {
                if batch_rescan {
                    return Some((index, Vec::new()));
                }
                let mount_changed = changed
                    .iter()
                    .filter(|(path, structural)| {
                        routing_interests[index]
                            .iter()
                            .any(|interest| interest.matches_event(path, **structural))
                    })
                    .map(|(path, structural)| (path.clone(), *structural))
                    .collect::<Vec<_>>();
                (!mount_changed.is_empty()).then_some((index, mount_changed))
            })
            .collect::<Vec<_>>();
        let mut startup_primary_error = None;

        for (index, mount_events) in affected {
            let mount_changed = mount_events
                .iter()
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>();
            let mount_structural = mount_events.iter().any(|(_, structural)| *structural);
            // A structural change can retarget a symlink. The raw backend identity may then match
            // only the old resolved interest, so promote it to an authoritative Rescan instead of
            // forwarding an incomplete Paths payload.
            let mount_rescan = batch_rescan || mount_structural;
            let exact_control_changed = mount_rescan
                || mount_changed.iter().any(|path| {
                    routing_interests[index]
                        .iter()
                        .any(|interest| interest.matches_exact_file(path))
                });
            let invalidation = if mount_rescan {
                WatchInvalidation::Rescan
            } else {
                WatchInvalidation::Paths(mount_changed.clone())
            };
            let initializing_eager = workers[index].session.is_none()
                && workers[index].spec.loading == DevLoading::Eager;
            let recovering_eager =
                is_recovering_eager_mount(recovery_batch, workers[index].spec.loading);
            if workers[index].session.is_none() && workers[index].spec.loading == DevLoading::Lazy {
                // Lazy mounts defer source regeneration until first request. Exact configuration
                // inputs still refresh the pending plan so that first request cannot use stale
                // compile settings.
                if !exact_control_changed {
                    if workers[index].lazy_retry_on_source
                        || workers[index].pending_candidate.is_some()
                    {
                        mounts[index]
                            .loading
                            .set_idle_phase(MountIdlePhase::Pending);
                    }
                    continue;
                }
                match refresh_mount(&workers[index].spec, &invalidation) {
                    Ok(DevMountRefresh::Invalidate { .. }) => {
                        let previous = workers[index].pending_candidate.take();
                        commit_or_recover!(commit_watch_backend_publication(
                            &watcher_lease,
                            previous,
                            if recovery_batch {
                                RefreshOutcome::RetryableFailure
                            } else {
                                RefreshOutcome::Superseded
                            },
                            || {
                                workers[index].lazy_retry_on_source = false;
                                mounts[index]
                                    .loading
                                    .set_idle_phase(MountIdlePhase::Pending);
                                emit_workspace_state(&mounts, None, event_handler.as_ref());
                            },
                        ));
                    }
                    Ok(DevMountRefresh::Candidate(candidate)) => {
                        if let Some(previous) = workers[index].pending_candidate.take() {
                            watcher_lease.finish_candidate(
                                previous,
                                if recovery_batch {
                                    RefreshOutcome::RetryableFailure
                                } else {
                                    RefreshOutcome::Superseded
                                },
                            );
                        }
                        workers[index].watch_interests = union_watch_interests(
                            &workers[index].watch_interests,
                            candidate.watch_interests(),
                        );
                        let desired_interests = workers
                            .iter()
                            .flat_map(|worker| worker.watch_interests.iter().cloned())
                            .collect::<Vec<_>>();
                        if let Err(message) = reconcile_runtime_watch_targets(
                            watcher.as_mut().expect("active watcher"),
                            &mut registered,
                            &desired_interests,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        ) {
                            watcher_lease.revoke();
                            watcher_lease
                                .finish_candidate(candidate, RefreshOutcome::RetryableFailure);
                            watcher.take();
                            registered.clear_after_backend_loss();
                            recreate_watcher = true;
                            schedule_watch_coverage_retry(
                                message.clone(),
                                &mut watch_retry_at,
                                &mut watch_retry_delay,
                                &mut watch_coverage_lost,
                                &mut last_watch_error,
                                &tx,
                                event_handler.as_ref(),
                            );
                            mounts[index]
                                .loading
                                .set_idle_phase(MountIdlePhase::Pending);
                            continue 'watch;
                        }
                        // Preserve the move-only candidate until the first request. No generated
                        // input, BuildSession, or visible generation is produced here.
                        let mut candidate = Some(candidate);
                        match watcher_lease.commit(|| {
                            workers[index].pending_candidate = candidate.take();
                            workers[index].lazy_retry_on_source = true;
                            mounts[index]
                                .loading
                                .set_idle_phase(MountIdlePhase::Pending);
                            emit_workspace_state(&mounts, None, event_handler.as_ref());
                        }) {
                            Ok(()) => {}
                            Err(rejection) => {
                                watcher_lease.finish_candidate(
                                    candidate.take().expect("rejected pending candidate"),
                                    match rejection {
                                        WatchCommitRejected::BackendLost => {
                                            RefreshOutcome::RetryableFailure
                                        }
                                        WatchCommitRejected::Stopped => RefreshOutcome::Aborted,
                                    },
                                );
                                if recover_after_watch_commit_rejection(
                                    rejection,
                                    &mut watcher,
                                    &watcher_lease,
                                    &mounts,
                                    &mut registered,
                                    &mut recreate_watcher,
                                    &mut watch_coverage_lost,
                                    &mut watch_retry_at,
                                    &mut watch_retry_delay,
                                    &mut last_watch_error,
                                    &tx,
                                    event_handler.as_ref(),
                                ) {
                                    return;
                                }
                                continue 'watch;
                            }
                        }
                    }
                    Ok(DevMountRefresh::RejectedCandidate {
                        watch_interests,
                        diagnostic,
                    }) => {
                        if let Some(previous) = workers[index].pending_candidate.take() {
                            watcher_lease.finish_candidate(
                                previous,
                                if recovery_batch {
                                    RefreshOutcome::RetryableFailure
                                } else {
                                    RefreshOutcome::Superseded
                                },
                            );
                        }
                        workers[index].watch_interests = union_watch_interests(
                            &workers[index].watch_interests,
                            &watch_interests,
                        );
                        let desired_interests = workers
                            .iter()
                            .flat_map(|worker| worker.watch_interests.iter().cloned())
                            .collect::<Vec<_>>();
                        if let Err(message) = reconcile_runtime_watch_targets(
                            watcher.as_mut().expect("active watcher"),
                            &mut registered,
                            &desired_interests,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        ) {
                            watcher_lease.revoke();
                            watcher.take();
                            registered.clear_after_backend_loss();
                            recreate_watcher = true;
                            schedule_watch_coverage_retry(
                                message.clone(),
                                &mut watch_retry_at,
                                &mut watch_retry_delay,
                                &mut watch_coverage_lost,
                                &mut last_watch_error,
                                &tx,
                                event_handler.as_ref(),
                            );
                            mounts[index]
                                .loading
                                .set_idle_phase(MountIdlePhase::Pending);
                            continue 'watch;
                        }
                        let spec = workers[index].spec.clone();
                        commit_or_recover!(report_refresh_failure(
                            &watcher_lease,
                            None,
                            &spec,
                            &mounts[index],
                            diagnostic,
                            &tx,
                            event_handler.as_ref(),
                            |error| {
                                workers[index].lazy_retry_on_source = true;
                                mounts[index]
                                    .loading
                                    .set_idle_phase(MountIdlePhase::Failed(error.to_owned()));
                                emit_workspace_state(&mounts, None, event_handler.as_ref());
                            },
                        ));
                    }
                    Ok(DevMountRefresh::RestartRequired { reason }) => {
                        if let Some(previous) = workers[index].pending_candidate.take() {
                            watcher_lease.finish_candidate(
                                previous,
                                if recovery_batch {
                                    RefreshOutcome::RetryableFailure
                                } else {
                                    RefreshOutcome::Superseded
                                },
                            );
                        }
                        let diagnostic = restart_required_diagnostic(reason);
                        let spec = workers[index].spec.clone();
                        commit_or_recover!(report_refresh_failure(
                            &watcher_lease,
                            None,
                            &spec,
                            &mounts[index],
                            diagnostic,
                            &tx,
                            event_handler.as_ref(),
                            |error| {
                                mounts[index]
                                    .loading
                                    .set_idle_phase(MountIdlePhase::Failed(error.to_owned()));
                                workers[index].lazy_retry_on_source = false;
                                emit_workspace_state(&mounts, None, event_handler.as_ref());
                            },
                        ));
                    }
                    Err(diagnostic) => {
                        if let Some(previous) = workers[index].pending_candidate.take() {
                            watcher_lease.finish_candidate(
                                previous,
                                if recovery_batch {
                                    RefreshOutcome::RetryableFailure
                                } else {
                                    RefreshOutcome::Superseded
                                },
                            );
                        }
                        let spec = workers[index].spec.clone();
                        commit_or_recover!(report_refresh_failure(
                            &watcher_lease,
                            None,
                            &spec,
                            &mounts[index],
                            diagnostic,
                            &tx,
                            event_handler.as_ref(),
                            |error| {
                                mounts[index]
                                    .loading
                                    .set_idle_phase(MountIdlePhase::Failed(error.to_owned()));
                                workers[index].lazy_retry_on_source = false;
                                emit_workspace_state(&mounts, None, event_handler.as_ref());
                            },
                        ));
                    }
                }
                continue;
            }
            if initializing_eager {
                workers[index].session = Some(create_mount_session(&workers[index].spec));
            }
            let workspace = workers[index].spec.name.clone();
            let mount_base = workers[index].spec.base_path.clone();
            if let Some(handler) = &event_handler {
                handler(ServerEvent::RebuildStart {
                    changed_paths: reported_watch_paths(&mount_changed),
                    workspace: workspace.clone(),
                    base_path: workspace.as_ref().map(|_| mount_base.clone()),
                });
            }
            let refresh = refresh_mount(&workers[index].spec, &invalidation);
            let mut invalidated = mount_changed.clone();
            match refresh {
                Ok(DevMountRefresh::Invalidate {
                    mut generated_paths,
                }) => {
                    let worker = &mut workers[index];
                    if mount_rescan {
                        worker
                            .session
                            .as_mut()
                            .expect("loaded session")
                            .invalidate_filesystem();
                    } else {
                        let structural = mount_structural || !generated_paths.is_empty();
                        invalidated.append(&mut generated_paths);
                        invalidated.sort();
                        invalidated.dedup();
                        worker
                            .session
                            .as_mut()
                            .expect("loaded session")
                            .invalidate_paths(&invalidated, structural);
                    }
                    let session = worker.session.take().expect("loaded session");
                    let spec = worker.spec.clone();
                    let outcome = rebuild_mount(
                        session,
                        &spec,
                        &mounts[index],
                        &tx,
                        &mounts[index].federation_tx,
                        &watcher_lease,
                        None,
                        false,
                        sty,
                        event_handler.as_ref(),
                        |commit| match commit {
                            MountRebuildCommit::Published { session, .. } => {
                                worker.session = Some(session);
                                mark_mount_rebuild_succeeded(
                                    &mounts,
                                    index,
                                    event_handler.as_ref(),
                                );
                            }
                            MountRebuildCommit::BuildFailed { session, error } => {
                                worker.session = Some(session);
                                if recovering_eager {
                                    if index == 0 {
                                        set_mount_idle_phase(
                                            &mounts[index],
                                            MountIdlePhase::Loaded,
                                        );
                                    } else {
                                        set_mount_idle_phase(
                                            &mounts[index],
                                            MountIdlePhase::Failed(error),
                                        );
                                    }
                                }
                            }
                        },
                    );
                    if completes_startup_fence && index == 0 {
                        match &outcome {
                            MountRebuildOutcome::Published(summary) => {
                                startup_ready = Some(Some(summary.clone()));
                            }
                            MountRebuildOutcome::BuildFailed if startup_primary_build_succeeded => {
                                startup_primary_error = Some(
                                    mounts[index]
                                        .published
                                        .read()
                                        .unwrap()
                                        .bundle
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| {
                                            "post-registration Rescan build failed".to_owned()
                                        }),
                                );
                            }
                            _ => {}
                        }
                    }
                    match outcome {
                        MountRebuildOutcome::BackendLost(session) => {
                            workers[index].session = Some(session);
                            report_pending_watch_backend_error(
                                &watcher_lease,
                                &mut last_watch_error,
                                &tx,
                                event_handler.as_ref(),
                            );
                            enter_watch_backend_recovery(
                                &mut watcher,
                                &watcher_lease,
                                &mounts,
                                &mut registered,
                                &mut recreate_watcher,
                                &mut watch_coverage_lost,
                                &mut watch_retry_at,
                                &mut watch_retry_delay,
                            );
                            continue 'watch;
                        }
                        MountRebuildOutcome::Stopped(session) => {
                            workers[index].session = Some(session);
                            return;
                        }
                        MountRebuildOutcome::Published(_) | MountRebuildOutcome::BuildFailed => {}
                    }
                }
                Ok(DevMountRefresh::Candidate(mut candidate)) => {
                    // Preliminary coverage must be confirmed before a candidate is allowed to
                    // allocate/write generated inputs.
                    workers[index].watch_interests = union_watch_interests(
                        &workers[index].watch_interests,
                        candidate.watch_interests(),
                    );
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher_lease.finish_candidate(candidate, RefreshOutcome::RetryableFailure);
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        continue 'watch;
                    }
                    let materialized = match candidate.materialize_unfinished() {
                        Ok(materialized) => materialized,
                        Err(diagnostic) => {
                            commit_or_recover!(report_refresh_failure(
                                &watcher_lease,
                                Some(candidate),
                                &workers[index].spec,
                                &mounts[index],
                                diagnostic,
                                &tx,
                                event_handler.as_ref(),
                                |error| {
                                    if completes_startup_fence && index == 0 {
                                        startup_primary_error = Some(error.to_owned());
                                    }
                                    if recovering_eager {
                                        set_mount_idle_phase(
                                            &mounts[index],
                                            MountIdlePhase::Failed(error.to_owned()),
                                        );
                                    }
                                },
                            ));
                            continue;
                        }
                    };
                    let DevMountMaterialization {
                        plan,
                        watch_interests,
                        generated_paths: _,
                    } = materialized;
                    if let Err(diagnostic) = validate_replacement_plan(&plan) {
                        commit_or_recover!(report_refresh_failure(
                            &watcher_lease,
                            Some(candidate),
                            &workers[index].spec,
                            &mounts[index],
                            diagnostic,
                            &tx,
                            event_handler.as_ref(),
                            |error| {
                                if completes_startup_fence && index == 0 {
                                    startup_primary_error = Some(error.to_owned());
                                }
                                if recovering_eager {
                                    set_mount_idle_phase(
                                        &mounts[index],
                                        MountIdlePhase::Failed(error.to_owned()),
                                    );
                                }
                            },
                        ));
                        continue;
                    }
                    let mut candidate_spec = workers[index].spec.clone();
                    candidate_spec.plan = Some(plan);
                    candidate_spec.watch_interests = watch_interests;
                    let candidate_interests = mount_watch_interests(&candidate_spec);
                    workers[index].watch_interests = union_watch_interests(
                        &workers[index].watch_interests,
                        &candidate_interests,
                    );
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher_lease.finish_candidate(candidate, RefreshOutcome::RetryableFailure);
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        continue 'watch;
                    }
                    let candidate_session = create_mount_session(&candidate_spec);
                    let committed_spec = candidate_spec.clone();
                    let committed_interests = candidate_interests.clone();
                    let outcome = rebuild_mount(
                        candidate_session,
                        &candidate_spec,
                        &mounts[index],
                        &tx,
                        &mounts[index].federation_tx,
                        &watcher_lease,
                        Some(candidate),
                        false,
                        sty,
                        event_handler.as_ref(),
                        |commit| match commit {
                            MountRebuildCommit::Published { session, .. } => {
                                workers[index].watch_interests = committed_interests;
                                workers[index].spec = committed_spec;
                                workers[index].session = Some(session);
                                mark_mount_rebuild_succeeded(
                                    &mounts,
                                    index,
                                    event_handler.as_ref(),
                                );
                            }
                            MountRebuildCommit::BuildFailed { session: _, error } => {
                                if recovering_eager {
                                    if index == 0 {
                                        set_mount_idle_phase(
                                            &mounts[index],
                                            MountIdlePhase::Loaded,
                                        );
                                    } else {
                                        set_mount_idle_phase(
                                            &mounts[index],
                                            MountIdlePhase::Failed(error),
                                        );
                                    }
                                }
                            }
                        },
                    );
                    if completes_startup_fence && index == 0 {
                        match &outcome {
                            MountRebuildOutcome::Published(summary) => {
                                startup_ready = Some(Some(summary.clone()));
                            }
                            MountRebuildOutcome::BuildFailed if startup_primary_build_succeeded => {
                                startup_primary_error = Some(
                                    mounts[index]
                                        .published
                                        .read()
                                        .unwrap()
                                        .bundle
                                        .error
                                        .clone()
                                        .unwrap_or_else(|| {
                                            "post-registration Rescan candidate build failed"
                                                .to_owned()
                                        }),
                                );
                            }
                            _ => {}
                        }
                    }
                    match outcome {
                        MountRebuildOutcome::BackendLost(_candidate_session) => {
                            report_pending_watch_backend_error(
                                &watcher_lease,
                                &mut last_watch_error,
                                &tx,
                                event_handler.as_ref(),
                            );
                            enter_watch_backend_recovery(
                                &mut watcher,
                                &watcher_lease,
                                &mounts,
                                &mut registered,
                                &mut recreate_watcher,
                                &mut watch_coverage_lost,
                                &mut watch_retry_at,
                                &mut watch_retry_delay,
                            );
                            continue 'watch;
                        }
                        MountRebuildOutcome::Stopped(_candidate_session) => return,
                        MountRebuildOutcome::Published(_) | MountRebuildOutcome::BuildFailed => {}
                    }
                }
                Ok(DevMountRefresh::RejectedCandidate {
                    watch_interests,
                    diagnostic,
                }) => {
                    workers[index].watch_interests =
                        union_watch_interests(&workers[index].watch_interests, &watch_interests);
                    let desired_interests = workers
                        .iter()
                        .flat_map(|worker| worker.watch_interests.iter().cloned())
                        .collect::<Vec<_>>();
                    if let Err(message) = reconcile_runtime_watch_targets(
                        watcher.as_mut().expect("active watcher"),
                        &mut registered,
                        &desired_interests,
                        &mut watch_retry_at,
                        &mut watch_retry_delay,
                        &mut last_watch_error,
                        &tx,
                        event_handler.as_ref(),
                    ) {
                        watcher_lease.revoke();
                        watcher.take();
                        registered.clear_after_backend_loss();
                        recreate_watcher = true;
                        schedule_watch_coverage_retry(
                            message,
                            &mut watch_retry_at,
                            &mut watch_retry_delay,
                            &mut watch_coverage_lost,
                            &mut last_watch_error,
                            &tx,
                            event_handler.as_ref(),
                        );
                        continue 'watch;
                    }
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &workers[index].spec,
                        &mounts[index],
                        diagnostic,
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            if completes_startup_fence && index == 0 {
                                startup_primary_error = Some(error.to_owned());
                            }
                            if recovering_eager {
                                set_mount_idle_phase(
                                    &mounts[index],
                                    MountIdlePhase::Failed(error.to_owned()),
                                );
                            }
                        },
                    ));
                }
                Ok(DevMountRefresh::RestartRequired { reason }) => {
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &workers[index].spec,
                        &mounts[index],
                        restart_required_diagnostic(reason),
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            if completes_startup_fence && index == 0 {
                                startup_primary_error = Some(error.to_owned());
                            }
                            if recovering_eager {
                                set_mount_idle_phase(
                                    &mounts[index],
                                    MountIdlePhase::Failed(error.to_owned()),
                                );
                            }
                        },
                    ));
                }
                Err(diagnostic) => {
                    commit_or_recover!(report_refresh_failure(
                        &watcher_lease,
                        None,
                        &workers[index].spec,
                        &mounts[index],
                        diagnostic,
                        &tx,
                        event_handler.as_ref(),
                        |error| {
                            if completes_startup_fence && index == 0 {
                                startup_primary_error = Some(error.to_owned());
                            }
                            if recovering_eager {
                                set_mount_idle_phase(
                                    &mounts[index],
                                    MountIdlePhase::Failed(error.to_owned()),
                                );
                            }
                        },
                    ));
                }
            }
        }
        let desired_interests = workers
            .iter()
            .flat_map(|worker| worker.watch_interests.iter().cloned())
            .collect::<Vec<_>>();
        if let Err(message) = reconcile_runtime_watch_targets(
            watcher.as_mut().expect("active watcher"),
            &mut registered,
            &desired_interests,
            &mut watch_retry_at,
            &mut watch_retry_delay,
            &mut last_watch_error,
            &tx,
            event_handler.as_ref(),
        ) {
            watcher_lease.revoke();
            watcher.take();
            registered.clear_after_backend_loss();
            recreate_watcher = true;
            schedule_watch_coverage_retry(
                message,
                &mut watch_retry_at,
                &mut watch_retry_delay,
                &mut watch_coverage_lost,
                &mut last_watch_error,
                &tx,
                event_handler.as_ref(),
            );
            continue 'watch;
        }
        if completes_startup_fence && registered.is_coverage_complete(&desired_interests) {
            if let Some(error) = startup_primary_error {
                let _ = ready_tx.send(Err(error));
                return;
            }
            let summary = startup_ready.take().expect("startup readiness is pending");
            if ready_tx.send(Ok(summary)).is_err() {
                return;
            }
        }
    }
}

pub const DEV_RESTART_REQUIRED_CODE: &str = "WAKE_DEV_RESTART_REQUIRED";

#[allow(clippy::result_large_err)]
fn refresh_mount(
    spec: &MountSpec,
    invalidation: &WatchInvalidation,
) -> Result<DevMountRefresh, Diagnostic> {
    if let Some(refresh) = &spec.deferred_refresh {
        return refresh(invalidation);
    }
    spec.refresh.as_ref().map_or_else(
        || {
            Ok(DevMountRefresh::Invalidate {
                generated_paths: Vec::new(),
            })
        },
        |refresh| {
            let plan = spec
                .plan
                .as_ref()
                .expect("a ready refresh always owns an accepted plan");
            refresh(plan, invalidation)
        },
    )
}

#[allow(clippy::result_large_err)]
fn validate_replacement_plan(candidate: &DevMountPlan) -> Result<(), Diagnostic> {
    if !candidate.file_system.is_file(&candidate.entry) {
        return Err(Diagnostic::error(format!(
            "candidate development entry does not exist: {}",
            candidate.entry.display()
        ))
        .with_code("WAKE_IO")
        .with_path(candidate.entry.to_string_lossy().into_owned()));
    }
    Ok(())
}

fn restart_required_diagnostic(reason: impl Into<String>) -> Diagnostic {
    Diagnostic::error(format!(
        "{}; restart the Wake development server",
        reason.into()
    ))
    .with_code(DEV_RESTART_REQUIRED_CODE)
}

fn report_refresh_failure(
    lease: &WatchBackendLease,
    candidate: Option<DevMountCandidate>,
    spec: &MountSpec,
    mount: &MountedAppState,
    diagnostic: Diagnostic,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
    on_commit: impl FnOnce(&str),
) -> Result<(), WatchCommitRejected> {
    let error = format_diagnostics(std::slice::from_ref(&diagnostic));
    commit_watch_backend_publication(lease, candidate, RefreshOutcome::RetryableFailure, || {
        mount.published.write().unwrap().bundle.error = Some(error.clone());
        if let Some(handler) = event_handler {
            handler(ServerEvent::Diagnostics {
                diagnostics: vec![diagnostic],
                sources: Vec::new(),
            });
        }
        let _ = tx.send(msg_error(&error, spec.name.as_deref()));
        on_commit(&error);
    })
}

fn report_watch_failure(
    message: String,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) {
    let diagnostic = Diagnostic::error(message).with_code("WAKE_WATCH");
    let error = format_diagnostics(std::slice::from_ref(&diagnostic));
    if let Some(handler) = event_handler {
        handler(ServerEvent::Diagnostics {
            diagnostics: vec![diagnostic],
            sources: Vec::new(),
        });
    }
    let _ = tx.send(msg_error(&error, None));
}

fn report_pending_watch_backend_error(
    lease: &WatchBackendLease,
    last_error: &mut Option<String>,
    tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) {
    let Some(message) = lease.take_error() else {
        return;
    };
    if last_error.as_deref() != Some(message.as_str()) {
        report_watch_failure(message.clone(), tx, event_handler);
        *last_error = Some(message);
    }
}

fn mount_watch_interests(spec: &MountSpec) -> Vec<WatchInterest> {
    let default_watch_dir = {
        let src = spec.root.join("src");
        if src.is_dir() { src } else { spec.root.clone() }
    };
    let mut interests = vec![
        WatchInterest::tree(default_watch_dir),
        WatchInterest::all_files_tree(spec.root.join("public")),
        WatchInterest::exact_file(spec.root.join("index.html")),
    ]
    .into_iter()
    .map(|interest| interest.resolve_against(&spec.root))
    .collect::<Vec<_>>();
    // Generated entries are driven by their source/control interests. Watching `.wake` outputs
    // would feed the writer back into the build loop. Direct user entries retain the convenient
    // default exact-file interest.
    if let Some(plan) = &spec.plan
        && !path_starts_with(&plan.entry, &spec.root.join(".wake"))
    {
        interests.push(WatchInterest::exact_file(plan.entry.clone()));
    }
    interests.extend(
        spec.watch_interests
            .iter()
            .map(|interest| interest.resolve_against(&spec.root)),
    );
    interests.sort();
    interests.dedup();
    interests
}

fn union_watch_interests(left: &[WatchInterest], right: &[WatchInterest]) -> Vec<WatchInterest> {
    let mut interests = left.iter().chain(right).cloned().collect::<Vec<_>>();
    interests.sort();
    interests.dedup();
    interests
}

fn watch_targets(interests: &[WatchInterest]) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut targets = BTreeMap::<PathBuf, RecursiveMode>::new();
    for (path, mode) in interests.iter().flat_map(WatchInterest::registrations) {
        targets
            .entry(path)
            .and_modify(|current| {
                if mode == RecursiveMode::Recursive {
                    *current = RecursiveMode::Recursive;
                }
            })
            .or_insert(mode);
    }
    let recursive_roots = targets
        .iter()
        .filter_map(|(path, mode)| (*mode == RecursiveMode::Recursive).then_some(path.clone()))
        .collect::<Vec<_>>();
    targets.retain(|path, _| {
        !recursive_roots
            .iter()
            .any(|ancestor| !paths_equal(path, ancestor) && path_starts_with(path, ancestor))
    });
    targets
}

/// Confirmed backend registrations. Entries are updated only after the corresponding watcher
/// operation succeeds; a desired plan is never recorded speculatively.
#[derive(Debug, Default)]
pub struct WatchRegistrationState {
    registered: BTreeMap<PathBuf, RecursiveMode>,
}

impl WatchRegistrationState {
    pub fn clear_after_backend_loss(&mut self) {
        self.registered.clear();
    }

    pub fn is_coverage_complete(&self, interests: &[WatchInterest]) -> bool {
        watch_targets(interests).iter().all(|(path, mode)| {
            self.registered
                .get(path)
                .is_some_and(|current| watch_mode_covers(*current, *mode))
        })
    }

    pub fn is_converged(&self, interests: &[WatchInterest]) -> bool {
        let desired = watch_targets(interests);
        desired.iter().all(|(path, mode)| {
            self.registered
                .get(path)
                .is_some_and(|current| watch_mode_covers(*current, *mode))
        }) && self
            .registered
            .keys()
            .all(|path| desired.contains_key(path))
    }
}

fn watch_mode_covers(current: RecursiveMode, desired: RecursiveMode) -> bool {
    current == desired
        || (current == RecursiveMode::Recursive && desired == RecursiveMode::NonRecursive)
}

#[derive(Debug, Default)]
pub struct WatchReconcileOutcome {
    pub coverage_changed: bool,
    pub cleanup_errors: Vec<String>,
}

#[derive(Debug)]
pub struct WatchReconcileError {
    pub message: String,
}

impl std::fmt::Display for WatchReconcileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WatchReconcileError {}

pub fn reconcile_watch_interests<W: Watcher>(
    watcher: &mut W,
    state: &mut WatchRegistrationState,
    interests: &[WatchInterest],
) -> Result<WatchReconcileOutcome, WatchReconcileError> {
    let desired = watch_targets(interests);
    let mut outcome = WatchReconcileOutcome::default();
    for (path, mode) in &desired {
        match state.registered.get(path).copied() {
            Some(current) if watch_mode_covers(current, *mode) => {}
            Some(current) => {
                watcher.unwatch(path).map_err(|error| WatchReconcileError {
                    message: format!("cannot update watch {}: {error}", path.display()),
                })?;
                state.registered.remove(path);
                if let Err(error) = watcher.watch(path, *mode) {
                    let rollback = watcher.watch(path, current);
                    if rollback.is_ok() {
                        state.registered.insert(path.clone(), current);
                    }
                    let rollback_note = rollback
                        .err()
                        .map(|rollback| {
                            format!("; restoring the previous watch also failed: {rollback}")
                        })
                        .unwrap_or_default();
                    return Err(WatchReconcileError {
                        message: format!(
                            "cannot update watch {}: {error}{rollback_note}",
                            path.display()
                        ),
                    });
                }
                state.registered.insert(path.clone(), *mode);
                outcome.coverage_changed = true;
            }
            None => {
                watcher
                    .watch(path, *mode)
                    .map_err(|error| WatchReconcileError {
                        message: format!("cannot watch {}: {error}", path.display()),
                    })?;
                state.registered.insert(path.clone(), *mode);
                outcome.coverage_changed = true;
            }
        }
    }
    let obsolete = state
        .registered
        .keys()
        .filter(|path| !desired.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    for path in obsolete {
        match watcher.unwatch(&path) {
            Ok(()) => {
                state.registered.remove(&path);
            }
            Err(error) => outcome.cleanup_errors.push(format!(
                "cannot remove obsolete watch {}: {error}",
                path.display()
            )),
        }
    }
    Ok(outcome)
}

fn reconcile_watch_targets<W: Watcher>(
    watcher: &mut W,
    state: &mut WatchRegistrationState,
    interests: &[WatchInterest],
) -> Result<(), String> {
    let outcome =
        reconcile_watch_interests(watcher, state, interests).map_err(|error| error.to_string())?;
    if outcome.cleanup_errors.is_empty() {
        Ok(())
    } else {
        Err(outcome.cleanup_errors.join("; "))
    }
}

struct MountBuildSession {
    generation: BuildGeneration,
    session: BuildSession,
}

impl MountBuildSession {
    fn build_current_generation(
        &mut self,
        request: BuildRequest,
    ) -> (&wake_bundler::BuildOutput, Arc<dyn FileSystem>) {
        let file_system = self.session.file_system_view();
        let output = self.session.build_current_ref(request);
        (output, file_system)
    }

    fn invalidate_filesystem(&mut self) {
        let generation = self.generation.advance_generation();
        let session_generation = self.session.invalidate_filesystem();
        debug_assert_eq!(generation, session_generation);
    }

    fn invalidate_paths(&mut self, paths: &[PathBuf], structural: bool) {
        let generation = self.generation.advance_generation();
        let session_generation = self.session.invalidate_paths(paths, structural);
        debug_assert_eq!(generation, session_generation);
    }
}

fn capture_diagnostic_sources(
    diagnostics: &[Diagnostic],
    file_system: &dyn FileSystem,
) -> Vec<DiagnosticSource> {
    let mut seen = BTreeSet::new();
    diagnostics
        .iter()
        .filter_map(|diagnostic| diagnostic.path.as_deref())
        .map(PathBuf::from)
        .filter_map(|path| {
            if !seen.insert(path.clone()) {
                return None;
            }
            file_system
                .read_to_string(&path)
                .ok()
                .map(|text| DiagnosticSource { path, text })
        })
        .collect()
}

fn create_mount_session(spec: &MountSpec) -> MountBuildSession {
    let plan = spec
        .plan
        .as_ref()
        .expect("a build session requires a materialized mount plan");
    let mut federation = FederationBuildPlan::default();
    if spec.federation.enabled {
        federation.remotes = spec.federation.remotes.clone();
        federation.shared = spec.federation.shared.clone();
        federation.shared_fallback_roots = spec
            .federation
            .shared_fallback_root
            .iter()
            .cloned()
            .collect();
        // Wake-native development always compiles a synthetic container entry. A host-only
        // application can have no public exposes while still owning the standalone application
        // loader and a lazy shared fallback, so it needs the same build-scoped registry slot as a
        // remote. `entry_export` remains the legacy single-entry path when no synthetic loader is
        // configured.
        let has_synthetic_container = !spec.federation.exposes.is_empty()
            || spec.federation.application_loader_export.is_some()
            || spec.federation.shared_fallback.is_some();
        if has_synthetic_container {
            federation.entry_export = Some(FederationEntryExport::build_scoped(
                &spec.federation.container_name,
                "./__wake_container__",
            ));
            federation.expose_roots = spec
                .federation
                .exposes
                .iter()
                .map(|expose| (expose.chunk_name.clone(), expose.key.as_str().to_owned()))
                .collect();
        } else if let Some(expose) = &spec.federation.entry_export {
            federation.entry_export = Some(FederationEntryExport::page_scoped(
                &spec.federation.container_name,
                expose,
            ));
        }
    }
    let generation = BuildGeneration::new(Arc::clone(&plan.file_system));
    let session = generation.retained_session(BundlerBuildOptions {
        project_root: Some(spec.root.clone()),
        // 别名（@/@@）+ define（dev 口径）须在首次 build 前固定，dev 与 build 一致。
        resolve: plan.resolve_options.clone(),
        define: plan.define.clone(),
        // Federation CSS belongs to the immutable remote asset closure. Other development CSS
        // remains injected by the non-extracting profile.
        extract_css: spec.federation.enabled,
        public_path: spec.base_path.clone(),
        source_map: true,
        css_in_js: true,
        code_splitting: true,
        jsx: JsxOptions {
            development: true,
            import_source: plan.jsx_import_source.clone(),
        },
        federation,
        target_env: plan.target_env.clone(),
        ..BundlerBuildOptions::default()
    });
    MountBuildSession {
        generation,
        session,
    }
}

fn set_mount_idle_phase(mount: &MountedAppState, phase: MountIdlePhase) {
    mount.loading.set_idle_phase(phase);
}

fn mark_mount_rebuild_succeeded(
    mounts: &[Arc<MountedAppState>],
    index: usize,
    event_handler: Option<&EventHandler>,
) {
    set_mount_idle_phase(&mounts[index], MountIdlePhase::Loaded);
    emit_workspace_state(mounts, None, event_handler);
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
        match mount.loading.phase() {
            MountLoadPhase::Loaded => loaded += 1,
            MountLoadPhase::Failed(_) | MountLoadPhase::Stopped(_) => failed_names.push(
                mount
                    .name
                    .clone()
                    .unwrap_or_else(|| mount.base_path.clone()),
            ),
            MountLoadPhase::Pending | MountLoadPhase::Queued(_) | MountLoadPhase::Building(_) => {}
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

/// Notify event kind accepted by the watcher. Per-mount [`WatchInterest`] routing decides whether
/// a path is a filtered source-tree member or an exact control input.
fn is_watch_event(ev: &notify::Event) -> bool {
    use notify::EventKind;
    matches!(
        ev.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
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
            | "mjs"
            | "cjs"
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
            // Media assets accepted by wake_bundler's asset loader.
            | "mp4"
            | "webm"
            | "mp3"
            | "wav"
            | "ogg"
            // 字体
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
    )
}

fn bundle_state_from_output(output: &wake_bundler::BuildOutput) -> BundleState {
    // JavaScript bytes remain exactly as emitted by the bundler. Source maps are served through
    // response headers rather than mutating those bytes with sourceMappingURL comments.
    let map = output.chunks[output.entry_chunk].source_map.clone();
    BundleState {
        js: output.bundle.clone(),
        // Non-entry chunks are keyed by their emitted file name because the runtime requests
        // `publicPath + file` directly.
        chunks: output
            .chunks
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != output.entry_chunk)
            .map(|(_, chunk)| (chunk.file_name.clone(), chunk.code.clone()))
            .collect(),
        assets: output
            .assets
            .iter()
            .map(|asset| (asset.file_name.clone(), asset.bytes.clone()))
            .collect(),
        map,
        chunk_maps: output
            .chunks
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != output.entry_chunk)
            .filter_map(|(_, chunk)| {
                chunk
                    .source_map
                    .clone()
                    .map(|map| (format!("{}.map", chunk.file_name), map))
            })
            .collect(),
        error: None,
    }
}

enum MountRebuildOutcome {
    Published(BuildSummary),
    BuildFailed,
    BackendLost(MountBuildSession),
    Stopped(MountBuildSession),
}

enum MountRebuildCommit {
    Published {
        session: MountBuildSession,
    },
    BuildFailed {
        session: MountBuildSession,
        error: String,
    },
}

/// 执行一次（增量）构建并更新共享状态 + 广播 Live Reload 事件。
fn rebuild_mount(
    mut session: MountBuildSession,
    spec: &MountSpec,
    mount: &MountedAppState,
    tx: &broadcast::Sender<String>,
    federation_tx: &broadcast::Sender<String>,
    lease: &WatchBackendLease,
    candidate: Option<DevMountCandidate>,
    first: bool,
    sty: Sty,
    event_handler: Option<&EventHandler>,
    on_commit: impl FnOnce(MountRebuildCommit),
) -> MountRebuildOutcome {
    let t = Instant::now();
    let entry = &spec
        .plan
        .as_ref()
        .expect("a mount build requires a materialized plan")
        .entry;
    let (out, generation_fs) = session.build_current_generation(BuildRequest::new(entry));
    let elapsed = t.elapsed();
    let dur = human_dur(elapsed);
    let sep = sty.dim("·");
    if out.has_errors() {
        let errs = out.diagnostics.iter().filter(|d| d.is_error()).count();
        let err = format_diagnostics(&out.diagnostics);
        let diagnostics = out
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
            .cloned()
            .map(|mut diagnostic| {
                if let Some(path) = diagnostic.path.as_deref() {
                    let path = PathBuf::from(path);
                    if !path.is_absolute() {
                        diagnostic.path = Some(spec.root.join(path).to_string_lossy().into_owned());
                    }
                }
                if let Some(workspace) = &spec.name {
                    diagnostic
                        .notes
                        .push(format!("Docs workspace: {workspace}"));
                }
                diagnostic
            })
            .collect::<Vec<_>>();
        let sources = capture_diagnostic_sources(&diagnostics, generation_fs.as_ref());
        let mut session = Some(session);
        match commit_watch_backend_publication(
            lease,
            candidate,
            RefreshOutcome::RetryableFailure,
            || {
                mount.published.write().unwrap().bundle.error = Some(err.clone());
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
                    handler(ServerEvent::Diagnostics {
                        diagnostics,
                        sources,
                    });
                }
                let _ = tx.send(msg_error(&err, spec.name.as_deref()));
                on_commit(MountRebuildCommit::BuildFailed {
                    session: session.take().expect("uncommitted build session"),
                    error: err,
                });
            },
        ) {
            Ok(()) => MountRebuildOutcome::BuildFailed,
            Err(WatchCommitRejected::BackendLost) => MountRebuildOutcome::BackendLost(
                session
                    .take()
                    .expect("rejected build preserves its session"),
            ),
            Err(WatchCommitRejected::Stopped) => MountRebuildOutcome::Stopped(
                session.take().expect("stopped build preserves its session"),
            ),
        }
    } else {
        let federation = if spec.federation.enabled {
            let previous = mount
                .published
                .read()
                .unwrap()
                .federation
                .current()
                .cloned();
            match federation::FederationSnapshot::assemble(
                &spec.federation,
                out,
                generation_fs,
                &spec.base_path,
                &spec.federation_updates_url,
                previous.as_ref(),
            ) {
                Ok(artifacts) => Some(artifacts),
                Err(error) => {
                    let error = format!("[FED_MANIFEST_SCHEMA] {error}");
                    let mut session = Some(session);
                    return match commit_watch_backend_publication(
                        lease,
                        candidate,
                        RefreshOutcome::RetryableFailure,
                        || {
                            mount.published.write().unwrap().bundle.error = Some(error.clone());
                            if let Some(handler) = event_handler {
                                handler(ServerEvent::Diagnostics {
                                    diagnostics: vec![
                                        Diagnostic::error(error.clone())
                                            .with_code("FED_MANIFEST_SCHEMA"),
                                    ],
                                    sources: Vec::new(),
                                });
                            }
                            let _ = tx.send(msg_error(&error, spec.name.as_deref()));
                            on_commit(MountRebuildCommit::BuildFailed {
                                session: session.take().expect("uncommitted build session"),
                                error,
                            });
                        },
                    ) {
                        Ok(()) => MountRebuildOutcome::BuildFailed,
                        Err(WatchCommitRejected::BackendLost) => MountRebuildOutcome::BackendLost(
                            session
                                .take()
                                .expect("rejected build preserves its session"),
                        ),
                        Err(WatchCommitRejected::Stopped) => MountRebuildOutcome::Stopped(
                            session.take().expect("stopped build preserves its session"),
                        ),
                    };
                }
            }
        } else {
            None
        };
        let summary = BuildSummary {
            modules: out.module_count,
            updated_modules: out.updated_module_count,
            cached_modules: out.cached_module_count,
            chunks: out.chunks.len(),
            assets: out.assets.len(),
            duration: dur.clone(),
            duration_ms: elapsed.as_secs_f64() * 1000.0,
        };
        let bundle = bundle_state_from_output(out);
        let html = load_html_template(
            &spec.root,
            &spec.base_path,
            spec.name.as_deref(),
            spec.federation.enabled && spec.federation.bootstrap.is_some(),
        );
        let mut session = Some(session);
        // Assembly above owns every fallible Federation/type/manifest operation. Only a complete
        // candidate reaches this single write critical section, so HTTP readers can never pair a
        // new development bundle with the previous Federation snapshot (or vice versa).
        match commit_watch_backend_publication(lease, candidate, RefreshOutcome::Committed, || {
            let federation_update =
                mount
                    .published
                    .write()
                    .unwrap()
                    .install(PublishedMountCandidate {
                        bundle,
                        html,
                        federation,
                    });
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
            if !first && !spec.federation.enabled {
                let _ = tx.send(msg_reload(spec.name.as_deref()));
            }
            if !first && let Some(update) = federation_update {
                publish_federation_build_update(
                    &update,
                    spec.name.as_deref(),
                    federation_tx,
                    tx,
                    event_handler,
                );
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
            on_commit(MountRebuildCommit::Published {
                session: session.take().expect("uncommitted build session"),
            });
        }) {
            Ok(()) => MountRebuildOutcome::Published(summary),
            Err(WatchCommitRejected::BackendLost) => MountRebuildOutcome::BackendLost(
                session
                    .take()
                    .expect("rejected build preserves its session"),
            ),
            Err(WatchCommitRejected::Stopped) => MountRebuildOutcome::Stopped(
                session.take().expect("stopped build preserves its session"),
            ),
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

async fn serve_client() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/javascript; charset=utf-8")
        .body(client_runtime())
}

/// 服务 HTML（含 SPA fallback：任何未知 GET 路径都回退到应用外壳）。
async fn serve_html(mount: &MountedAppState) -> HttpResponse {
    let html = mount.published.read().unwrap().html.clone();
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
    if req.method() != actix_web::http::Method::GET && req.method() != actix_web::http::Method::HEAD
    {
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
    if let Err(error) = ensure_mount_ready(&mount, &data.stop).await {
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

    {
        // All in-memory routes for this request are selected from one published generation. The
        // guard spans both Federation and ordinary bundle lookups so an install cannot interleave
        // between them.
        let published = mount.published.read().unwrap();
        match published.federation.route(&rel) {
            federation::FederationRouteLookup::Found(route) => {
                return federation_route_response(
                    route,
                    rel == "wake-federation.json",
                    req.method() == actix_web::http::Method::HEAD,
                );
            }
            federation::FederationRouteLookup::Gone {
                cursor,
                expired_build_id,
            } => {
                return federation_gone_response(
                    cursor,
                    expired_build_id,
                    req.method() == actix_web::http::Method::HEAD,
                );
            }
            federation::FederationRouteLookup::Missing => {
                return HttpResponse::NotFound()
                    .content_type("text/plain; charset=utf-8")
                    .body("wake dev: Federation artifact is not published");
            }
            federation::FederationRouteLookup::NotFederation => {}
        }

        let bundle = &published.bundle;
        if rel == "bundle.js" {
            let map_url = bundle
                .map
                .as_ref()
                .map(|_| format!("{}bundle.js.map", mount.base_path));
            return javascript_response(bundle.js.clone(), map_url.as_deref());
        }
        if rel == "bundle.js.map" {
            return match bundle.map.clone() {
                Some(map) => HttpResponse::Ok()
                    .content_type("application/json; charset=utf-8")
                    .insert_header(("Cache-Control", "no-cache"))
                    .body(map),
                None => HttpResponse::NotFound().body("no source map"),
            };
        }

        if let Some(map) = bundle.chunk_maps.get(&rel).cloned() {
            return HttpResponse::Ok()
                .content_type("application/json; charset=utf-8")
                .insert_header(("Cache-Control", "no-cache"))
                .body(map);
        }

        // ② chunk（内存）
        if let Some(code) = bundle.chunks.get(&rel).cloned() {
            let map_file = format!("{rel}.map");
            let map_url = bundle
                .chunk_maps
                .contains_key(&map_file)
                .then(|| format!("{}{map_file}", mount.base_path));
            return javascript_response(code, map_url.as_deref());
        }
        // ③ 资源产物（内存）
        if let Some(bytes) = bundle.assets.get(&rel).cloned() {
            return HttpResponse::Ok()
                .content_type(mime_for(&rel))
                .insert_header(("Cache-Control", "no-cache"))
                .body(bytes);
        }
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

async fn ensure_mount_ready(mount: &MountedAppState, stop: &StopSignal) -> Result<(), String> {
    ensure_loading_ready(&mount.loading, stop).await
}

async fn ensure_loading_ready(
    loading: &MountLoadingState,
    stop: &StopSignal,
) -> Result<(), String> {
    // Subscribe before the first state read. `watch::Receiver::changed` remembers a version
    // published between that read and the await, so the readiness protocol cannot lose a wakeup.
    let mut phase_changed = loading.subscribe();
    let mut stop_changed = stop.subscribe();
    loop {
        if stop.is_requested() {
            return Err("Wake development server is stopping".to_owned());
        }
        match loading.poll_readiness() {
            MountReadiness::Ready => return Ok(()),
            MountReadiness::Failed(error) => return Err(error),
            MountReadiness::Enqueue(ticket) => {
                loading.enqueue(ticket);
                continue;
            }
            MountReadiness::Wait => {}
        }
        tokio::select! {
            result = phase_changed.changed() => {
                if result.is_err() {
                    return Err("Wake workspace loader stopped".to_owned());
                }
            }
            result = stop_changed.changed() => {
                if result.is_err() || *stop_changed.borrow_and_update() {
                    return Err("Wake development server is stopping".to_owned());
                }
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
async fn live_reload_ws_handler(
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
        .and_then(|mount| mount.published.read().unwrap().bundle.error.clone());

    actix_web::rt::spawn(async move {
        // 连接即同步当前状态。
        let first = match init {
            Some(err) => msg_error(
                &err,
                (!requested_mount.is_empty()).then_some(requested_mount.as_str()),
            ),
            None => msg_ready(),
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

/// Federation update and snapshot-lease transport. Server broadcasts remain complete validated
/// `wake.federation.dev-update.v1` frames. Each browser replaces its bounded build lease set with
/// `wake.federation.dev-lease.v1`; ownership is released when this socket closes.
async fn federation_ws_handler(
    req: HttpRequest,
    body: web::Payload,
    data: web::Data<AppState>,
) -> Result<HttpResponse, actix_web::Error> {
    let requested_remote = req
        .query_string()
        .split('&')
        .find_map(|pair| pair.strip_prefix("remote="))
        .and_then(safe_request_relative)
        .unwrap_or_default();
    if requested_remote.is_empty() {
        return Ok(HttpResponse::BadRequest()
            .content_type("text/plain; charset=utf-8")
            .body("wake dev: Federation update socket requires an exact remote"));
    }
    let Some(mount) = data.mounts.iter().find_map(|mount| {
        let cursor = mount.published.read().unwrap().federation.cursor()?;
        (cursor.remote.as_str() == requested_remote).then(|| Arc::clone(mount))
    }) else {
        return Ok(HttpResponse::NotFound()
            .content_type("text/plain; charset=utf-8")
            .body("wake dev: Federation remote is not mounted"));
    };
    let (response, mut session, mut stream) = actix_ws::handle(&req, body)?;
    let mut rx = mount.federation_tx.subscribe();

    actix_web::rt::spawn(async move {
        let mut leases = BTreeSet::new();
        loop {
            tokio::select! {
                biased;
                incoming = stream.next() => match incoming {
                    Some(Ok(actix_ws::Message::Ping(payload))) => {
                        let _ = session.pong(&payload).await;
                    }
                    Some(Ok(actix_ws::Message::Text(message))) => {
                        let lease = decode_federation_lease(&message, &requested_remote);
                        let (build_ids, requested) = match lease {
                            Ok(lease) => lease,
                            Err(reason) => {
                                if let Some(cursor) = current_federation_cursor(&mount) {
                                    let _ = send_federation_reload(
                                        &mut session,
                                        cursor,
                                        None,
                                        reason,
                                    ).await;
                                }
                                break;
                            }
                        };
                        let replacement = {
                            let mut published = mount.published.write().unwrap();
                            published.federation.replace_leases(&leases, &requested)
                        };
                        match replacement {
                            Ok(cursor) => {
                                leases = requested;
                                let ack = DevLeaseMessage::lease_ack(
                                    cursor.remote,
                                    build_ids,
                                    cursor.current_build_id,
                                    cursor.generation,
                                );
                                if send_federation_control(&mut session, &ack).await.is_err() {
                                    break;
                                }
                            }
                            Err(federation::FederationLeaseError::UnknownBuild(build_id)) => {
                                if let Some(cursor) = current_federation_cursor(&mount) {
                                    let _ = send_federation_reload(
                                        &mut session,
                                        cursor,
                                        Some(build_id),
                                        DevLeaseReloadReason::BuildGone,
                                    ).await;
                                }
                                break;
                            }
                            Err(federation::FederationLeaseError::TooManyBuilds) => {
                                if let Some(cursor) = current_federation_cursor(&mount) {
                                    let _ = send_federation_reload(
                                        &mut session,
                                        cursor,
                                        None,
                                        DevLeaseReloadReason::LeaseLimit,
                                    ).await;
                                }
                                break;
                            }
                            Err(federation::FederationLeaseError::NoSnapshot) => break,
                        }
                    }
                    Some(Ok(actix_ws::Message::Close(_))) | None => break,
                    Some(Err(_)) => break,
                    Some(Ok(message)) if invalid_federation_non_text_frame(&message) => {
                        if let Some(cursor) = current_federation_cursor(&mount) {
                            let _ = send_federation_reload(
                                &mut session,
                                cursor,
                                None,
                                DevLeaseReloadReason::InvalidLease,
                            ).await;
                        }
                        break;
                    }
                    Some(Ok(actix_ws::Message::Pong(_)
                        | actix_ws::Message::Nop)) => {}
                    Some(Ok(_)) => {}
                },
                broadcasted = rx.recv() => match broadcasted {
                    Ok(message) => {
                        let accepted = accepts_federation_update(&message, &requested_remote);
                        if accepted && session.text(message).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(cursor) = current_federation_cursor(&mount) {
                            let _ = send_federation_reload(
                                &mut session,
                                cursor,
                                None,
                                DevLeaseReloadReason::UpdateLagged,
                            ).await;
                        }
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
            }
        }
        mount
            .published
            .write()
            .unwrap()
            .federation
            .release_leases(&leases);
        let _ = session.close(None).await;
    });
    Ok(response)
}

fn invalid_federation_non_text_frame(message: &actix_ws::Message) -> bool {
    matches!(
        message,
        actix_ws::Message::Binary(_) | actix_ws::Message::Continuation(_)
    )
}

fn current_federation_cursor(
    mount: &MountedAppState,
) -> Option<federation::FederationSnapshotCursor> {
    mount.published.read().unwrap().federation.cursor()
}

fn decode_federation_lease(
    message: &str,
    requested_remote: &str,
) -> Result<
    (
        Vec<wake_federation_contract::BuildId>,
        BTreeSet<wake_federation_contract::BuildId>,
    ),
    DevLeaseReloadReason,
> {
    if message.len() > FEDERATION_LEASE_FRAME_MAX_BYTES {
        return Err(DevLeaseReloadReason::InvalidLease);
    }
    let lease = serde_json::from_str::<DevLeaseMessage>(message)
        .map_err(|_| DevLeaseReloadReason::InvalidLease)?;
    let DevLeaseMessage::Lease {
        remote, build_ids, ..
    } = &lease
    else {
        return Err(DevLeaseReloadReason::InvalidLease);
    };
    if build_ids.len() > FEDERATION_DEV_MAX_BUILD_LEASES {
        return Err(DevLeaseReloadReason::LeaseLimit);
    }
    lease
        .validate()
        .map_err(|_| DevLeaseReloadReason::InvalidLease)?;
    if remote.as_str() != requested_remote {
        return Err(DevLeaseReloadReason::InvalidLease);
    }
    let build_ids = build_ids.clone();
    let canonical = build_ids.iter().cloned().collect();
    Ok((build_ids, canonical))
}

async fn send_federation_control(
    session: &mut actix_ws::Session,
    control: &DevLeaseMessage,
) -> Result<(), actix_ws::Closed> {
    debug_assert!(control.validate().is_ok());
    let message = serde_json::to_string(control).expect("typed Federation control must serialize");
    session.text(message).await
}

async fn send_federation_reload(
    session: &mut actix_ws::Session,
    cursor: federation::FederationSnapshotCursor,
    expired_build_id: Option<wake_federation_contract::BuildId>,
    reason: DevLeaseReloadReason,
) -> Result<(), actix_ws::Closed> {
    let control = DevLeaseMessage::full_reload(
        cursor.remote,
        cursor.current_build_id,
        cursor.generation,
        expired_build_id,
        reason,
    );
    send_federation_control(session, &control).await
}

fn accepts_federation_update(message: &str, requested_remote: &str) -> bool {
    serde_json::from_str::<DevUpdate>(message)
        .ok()
        .filter(|update| update.validate().is_ok())
        .is_some_and(|update| {
            requested_remote.is_empty() || update.remote.as_str() == requested_remote
        })
}

// ======================================================================
// 入口 / HTML / 消息
// ======================================================================

/// 加载 HTML 外壳：优先项目 `public/index.html` / `index.html`，注入 Live Reload client；
/// 无则生成默认外壳。
fn load_html_template(
    root: &Path,
    base_path: &str,
    mount: Option<&str>,
    federation_bootstrap: bool,
) -> String {
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
            if federation_bootstrap {
                inject_federation_bootstrap(&mut html, base_path);
            }
            return html;
        }
    }
    default_html(base_path, mount, federation_bootstrap)
}

fn default_html(base_path: &str, mount: Option<&str>, federation_bootstrap: bool) -> String {
    let mount = format!("\"{}\"", json_escape(mount.unwrap_or("")));
    let mut html = format!(
        "<!doctype html><html lang=\"zh-CN\"><head><meta charset=\"utf-8\"/>\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\
         <title>wake dev</title>\
         <script>window.__WAKE_MOUNT__={mount}</script>\
         <script src=\"/__wake/client.js\"></script></head>\
         <body><div id=\"root\"></div><script src=\"{base_path}bundle.js\"></script></body></html>"
    );
    if federation_bootstrap {
        inject_federation_bootstrap(&mut html, base_path);
    }
    html
}

fn inject_federation_bootstrap(html: &mut String, base_path: &str) {
    let Some(bundle_position) = html.find("bundle.js") else {
        return;
    };
    let Some(script_start) = html[..bundle_position].rfind("<script") else {
        return;
    };
    let Some(relative_tag_end) = html[script_start..].find('>') else {
        return;
    };
    let tag_end = script_start + relative_tag_end;
    let Some(relative_script_end) = html[tag_end..].find("</script>") else {
        return;
    };
    let script_end = tag_end + relative_script_end + "</script>".len();
    html.replace_range(
        script_start..script_end,
        &format!(
            "<script type=\"module\" src=\"{base_path}@wake/federation/bootstrap.mjs\"></script>"
        ),
    );
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum LiveReloadMessage<'a> {
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "reload")]
    Reload { mount: &'a str },
    #[serde(rename = "error")]
    Error { message: &'a str, mount: &'a str },
}

fn encode_live_reload(message: LiveReloadMessage<'_>) -> String {
    serde_json::to_string(&message).expect("Live Reload messages contain only serializable strings")
}

fn msg_ready() -> String {
    encode_live_reload(LiveReloadMessage::Ready)
}

fn msg_error(err: &str, mount: Option<&str>) -> String {
    encode_live_reload(LiveReloadMessage::Error {
        message: err,
        mount: mount.unwrap_or(""),
    })
}

fn msg_reload(mount: Option<&str>) -> String {
    encode_live_reload(LiveReloadMessage::Reload {
        mount: mount.unwrap_or(""),
    })
}

fn javascript_response(code: String, source_map_url: Option<&str>) -> HttpResponse {
    let mut response = HttpResponse::Ok();
    response
        .content_type("application/javascript; charset=utf-8")
        .insert_header(("Cache-Control", "no-cache"));
    if let Some(source_map_url) = source_map_url {
        response.insert_header(("SourceMap", source_map_url));
    }
    response.body(code)
}

fn federation_route_response(
    route: federation::FederationRoute,
    manifest: bool,
    head: bool,
) -> HttpResponse {
    let mut response = HttpResponse::Ok();
    response
        .content_type(route.mime)
        .insert_header((
            "Cache-Control",
            if manifest { "no-store" } else { "no-cache" },
        ))
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .insert_header(("Cross-Origin-Resource-Policy", "cross-origin"))
        .insert_header(("Content-Length", route.bytes.len().to_string()));
    if let Some(source_map_url) = route.source_map_url {
        response.insert_header(("SourceMap", source_map_url));
    }
    if head {
        // `finish()` advertises an implicit zero-length body and Actix replaces the explicit
        // resource length with `0`. A sized empty stream preserves the GET representation length
        // on the wire while still yielding no response bytes for the HEAD preflight.
        response.body(actix_web::body::SizedStream::new(
            route.bytes.len() as u64,
            futures_util::stream::empty::<Result<web::Bytes, std::convert::Infallible>>(),
        ))
    } else {
        response.body(route.bytes)
    }
}

fn federation_gone_response(
    cursor: federation::FederationSnapshotCursor,
    expired_build_id: wake_federation_contract::BuildId,
    head: bool,
) -> HttpResponse {
    let control = DevLeaseMessage::full_reload(
        cursor.remote.clone(),
        cursor.current_build_id.clone(),
        cursor.generation,
        Some(expired_build_id.clone()),
        DevLeaseReloadReason::BuildGone,
    );
    let body = serde_json::to_vec(&control).expect("typed Federation control must serialize");
    let mut response = HttpResponse::Gone();
    response
        .content_type("application/json; charset=utf-8")
        .insert_header(("Cache-Control", "no-store"))
        .insert_header(("Access-Control-Allow-Origin", "*"))
        .insert_header((
            "Access-Control-Expose-Headers",
            FEDERATION_CONTROL_EXPOSE_HEADERS,
        ))
        .insert_header(("Cross-Origin-Resource-Policy", "cross-origin"))
        .insert_header((
            FEDERATION_CONTROL_HEADER,
            FEDERATION_DEV_LEASE_SCHEMA_VERSION,
        ))
        .insert_header((FEDERATION_ACTION_HEADER, "full-reload"))
        .insert_header((FEDERATION_REMOTE_HEADER, cursor.remote.as_str()))
        .insert_header((
            FEDERATION_CURRENT_BUILD_HEADER,
            cursor.current_build_id.as_str(),
        ))
        .insert_header((FEDERATION_GENERATION_HEADER, cursor.generation.to_string()))
        .insert_header((FEDERATION_EXPIRED_BUILD_HEADER, expired_build_id.as_str()))
        .insert_header((FEDERATION_REASON_HEADER, "build-gone"))
        .insert_header(("Content-Length", body.len().to_string()));
    if head {
        response.body(actix_web::body::SizedStream::new(
            body.len() as u64,
            futures_util::stream::empty::<Result<web::Bytes, std::convert::Infallible>>(),
        ))
    } else {
        response.body(body)
    }
}

fn msg_federation_update(update: &DevUpdate) -> Result<String, serde_json::Error> {
    serde_json::to_string(update)
}

fn emit_federation_server_event(update: &DevUpdate, handler: Option<&EventHandler>) {
    let Some(handler) = handler else { return };
    handler(ServerEvent::FederationUpdated {
        remote: update.remote.as_str().to_owned(),
        old_build_id: update
            .old_build_id
            .as_ref()
            .map(|build_id| build_id.as_str().to_owned()),
        new_build_id: update.new_build_id.as_str().to_owned(),
        changed_exposes: update
            .changed_exposes
            .iter()
            .map(|expose| expose.as_str().to_owned())
            .collect(),
        types_hash: update.types_hash.clone(),
        action: update.action,
    });
}

fn publish_federation_build_update(
    update: &DevUpdate,
    mount: Option<&str>,
    federation_tx: &broadcast::Sender<String>,
    local_tx: &broadcast::Sender<String>,
    event_handler: Option<&EventHandler>,
) {
    if let Ok(message) = msg_federation_update(update) {
        let _ = federation_tx.send(message.clone());
        match update.action {
            DevUpdateAction::FullReload => {
                let _ = local_tx.send(msg_reload(mount));
            }
            DevUpdateAction::IsolatedRemount => {
                // The local preview shares the browser-update socket. Forward the same valid
                // Federation frame so its broker can remount without affecting remote consumers
                // connected to the dedicated Federation transport.
                let _ = local_tx.send(message);
            }
            DevUpdateAction::TypesOnly => {}
        }
    }
    emit_federation_server_event(update, event_handler);
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

/// Live Reload 浏览器端运行时：连接 WS，处理整页刷新 / error overlay / 断连重连。
const CLIENT_RUNTIME_TEMPLATE: &str = r#"(function () {
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
  function handleFederationUpdate(message) {
    var broker = window[Symbol.for("wake.federation.v1")];
    if (!broker || typeof broker.applyDevUpdate !== "function") {
      if (message.action === "types-only") { clearError(); return; }
      location.reload();
      return;
    }
    var update;
    try {
      update = broker.applyDevUpdate(message);
    } catch (error) {
      console.error("[Wake Federation] rejected development update", error);
      location.reload();
      return;
    }
    clearError();
    if (update.action === "types-only") return;
    if (update.action === "full-reload") { location.reload(); return; }
    if (update.action === "isolated-remount" && typeof CustomEvent === "function" &&
        typeof window.dispatchEvent === "function") {
      window.dispatchEvent(new CustomEvent("wake:federation:isolated-remount", { detail: update }));
      return;
    }
    location.reload();
  }
  function connect() {
    var proto = location.protocol === "https:" ? "wss" : "ws";
    var mount = window.__WAKE_MOUNT__ || "";
    var ws = new WebSocket(proto + "://" + location.host + "__WAKE_LIVE_RELOAD_ENDPOINT__?mount=" + encodeURIComponent(mount));
    ws.onmessage = function (e) {
      var m;
      try { m = JSON.parse(e.data); } catch (_) { return; }
      if (m.mount != null && m.mount !== mount) return;
      if (m.schemaVersion === "wake.federation.dev-update.v1") { handleFederationUpdate(m); }
      else if (m.type === "reload") { clearError(); location.reload(); }
      else if (m.type === "error") { showError(m.message); }
      else if (m.type === "ready") { clearError(); }
    };
    ws.onclose = function () { setTimeout(connect, 1000); };
    ws.onerror = function () { try { ws.close(); } catch (_) {} };
  }
  connect();
})();
"#;

fn client_runtime() -> String {
    CLIENT_RUNTIME_TEMPLATE.replace("__WAKE_LIVE_RELOAD_ENDPOINT__", LIVE_RELOAD_ENDPOINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{FutureExt as _, SinkExt as _};
    use std::future::Future as _;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Barrier;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll};

    use wake_bundler::{BuildOutput, ChunkKind, OutputChunk};
    use wake_common::MemoryFileSystem;

    static NETWORK_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct AdvancingSourceFileSystem {
        base: MemoryFileSystem,
        source_path: PathBuf,
        first_source: String,
        later_source: String,
        source_reads: AtomicUsize,
    }

    impl AdvancingSourceFileSystem {
        fn new(source_path: PathBuf, first_source: &str, later_source: &str) -> Self {
            let base = MemoryFileSystem::new();
            base.insert(&source_path, first_source.as_bytes().to_vec());
            Self {
                base,
                source_path: wake_common::fs::normalize(&source_path),
                first_source: first_source.to_owned(),
                later_source: later_source.to_owned(),
                source_reads: AtomicUsize::new(0),
            }
        }

        fn source_read_count(&self) -> usize {
            self.source_reads.load(Ordering::SeqCst)
        }

        fn is_source(&self, path: &Path) -> bool {
            wake_common::fs::normalize(path) == self.source_path
        }
    }

    impl FileSystem for AdvancingSourceFileSystem {
        fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
            self.base.canonicalize(path)
        }

        fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            if !self.is_source(path) {
                return self.base.read_to_string(path);
            }
            let read = self.source_reads.fetch_add(1, Ordering::SeqCst);
            Ok(if read == 0 {
                self.first_source.clone()
            } else {
                self.later_source.clone()
            })
        }

        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            self.base.read(path)
        }

        fn exists(&self, path: &Path) -> bool {
            self.base.exists(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            self.base.is_file(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.base.is_dir(path)
        }

        fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
            self.base.read_dir(path)
        }
    }

    fn lock_network_test() -> std::sync::MutexGuard<'static, ()> {
        NETWORK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn refresh_candidate_completion_is_move_only_and_exactly_once() {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&outcomes);
        let candidate = DevMountCandidate::new(
            Vec::new(),
            || Err(Diagnostic::error("unused")),
            move |outcome| captured.lock().unwrap().push(outcome),
        );
        candidate.finish(RefreshOutcome::Committed);
        assert_eq!(*outcomes.lock().unwrap(), vec![RefreshOutcome::Committed]);

        let captured = Arc::clone(&outcomes);
        let candidate = DevMountCandidate::new(
            Vec::new(),
            || Err(Diagnostic::error("unused")),
            move |outcome| captured.lock().unwrap().push(outcome),
        );
        drop(candidate);
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![RefreshOutcome::Committed, RefreshOutcome::Aborted]
        );
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn materialization_failure_completes_as_retryable_once() {
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&outcomes);
        let mut candidate = DevMountCandidate::new(
            vec![WatchInterest::tree("candidate-source")],
            || Err(Diagnostic::error("materialization failed")),
            move |outcome| captured.lock().unwrap().push(outcome),
        );

        assert_eq!(candidate.watch_interests().len(), 1);
        assert!(candidate.materialize().is_err());
        drop(candidate);
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![RefreshOutcome::RetryableFailure]
        );
    }

    #[allow(clippy::result_large_err)]
    fn candidate_recording(outcomes: &Arc<Mutex<Vec<RefreshOutcome>>>) -> DevMountCandidate {
        let outcomes = Arc::clone(outcomes);
        DevMountCandidate::new(
            Vec::new(),
            || -> Result<DevMountMaterialization, Diagnostic> {
                panic!("publication-fence tests do not materialize the candidate")
            },
            move |outcome| outcomes.lock().unwrap().push(outcome),
        )
    }

    #[test]
    fn backend_revocation_rejects_blocked_publication_and_successor_can_publish() {
        let failed_generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(StopSignal::new());
        let lease = WatchBackendLease::new(1, Arc::clone(&failed_generation), Arc::clone(&stop));
        let published = Arc::new(AtomicUsize::new(1));
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_lease = lease.clone();
        let worker_published = Arc::clone(&published);
        let worker_outcomes = Arc::clone(&outcomes);
        let worker = std::thread::spawn(move || {
            let candidate = candidate_recording(&worker_outcomes);
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            commit_watch_backend_publication(
                &worker_lease,
                Some(candidate),
                RefreshOutcome::Committed,
                || worker_published.store(2, Ordering::Release),
            )
        });

        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        lease.revoke();
        release_tx.send(()).unwrap();
        assert_eq!(
            worker.join().unwrap(),
            Err(WatchCommitRejected::BackendLost)
        );
        assert_eq!(published.load(Ordering::Acquire), 1);
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![RefreshOutcome::RetryableFailure]
        );

        let successor = WatchBackendLease::new(2, failed_generation, stop);
        commit_watch_backend_publication(
            &successor,
            Some(candidate_recording(&outcomes)),
            RefreshOutcome::Committed,
            || published.store(3, Ordering::Release),
        )
        .unwrap();
        assert_eq!(published.load(Ordering::Acquire), 3);
        assert_eq!(
            *outcomes.lock().unwrap(),
            vec![RefreshOutcome::RetryableFailure, RefreshOutcome::Committed]
        );
        assert!(!is_current_watch_generation(&successor, 1));
        assert!(is_current_watch_generation(&successor, 2));
    }

    #[test]
    fn revocation_is_visible_before_waiting_for_linearized_commit() {
        let lease =
            WatchBackendLease::new(1, Arc::new(AtomicU64::new(0)), Arc::new(StopSignal::new()));
        let (commit_entered_tx, commit_entered_rx) = mpsc::channel();
        let (release_commit_tx, release_commit_rx) = mpsc::channel();
        let commit_lease = lease.clone();
        let commit = std::thread::spawn(move || {
            commit_lease.commit(|| {
                commit_entered_tx.send(()).unwrap();
                release_commit_rx.recv().unwrap();
            })
        });
        commit_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoke_lease = lease.clone();
        let revoke = std::thread::spawn(move || {
            revoke_lease.revoke();
            revoked_tx.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        while !lease.is_revoked() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(lease.is_revoked(), "revocation watermark was not published");
        assert!(matches!(
            revoked_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        release_commit_tx.send(()).unwrap();
        assert_eq!(commit.join().unwrap(), Ok(()));
        revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        revoke.join().unwrap();
    }

    #[test]
    fn shutdown_rejects_publication_and_aborts_candidate() {
        let stop = Arc::new(StopSignal::new());
        let lease = WatchBackendLease::new(1, Arc::new(AtomicU64::new(0)), Arc::clone(&stop));
        let published = AtomicUsize::new(1);
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        stop.request();

        assert_eq!(
            commit_watch_backend_publication(
                &lease,
                Some(candidate_recording(&outcomes)),
                RefreshOutcome::Committed,
                || published.store(2, Ordering::Release),
            ),
            Err(WatchCommitRejected::Stopped)
        );
        assert_eq!(published.load(Ordering::Acquire), 1);
        assert_eq!(*outcomes.lock().unwrap(), vec![RefreshOutcome::Aborted]);
    }

    #[test]
    fn rescan_notification_revokes_lease_before_it_is_enqueued() {
        let lease =
            WatchBackendLease::new(7, Arc::new(AtomicU64::new(0)), Arc::new(StopSignal::new()));
        let (tx, rx) = mpsc::channel();
        let event =
            notify::Event::new(notify::EventKind::Other).set_flag(notify::event::Flag::Rescan);

        forward_dev_watch_notification(&tx, &lease, Ok(event));

        assert!(lease.is_revoked());
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WatchBackendNotification::Rescan { generation: 7 }
        ));
    }

    #[test]
    fn backend_error_is_retained_by_failed_generation_and_stale_for_successor() {
        let failed_generation = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(StopSignal::new());
        let failed = WatchBackendLease::new(3, Arc::clone(&failed_generation), Arc::clone(&stop));
        let (tx, rx) = mpsc::channel();

        forward_dev_watch_notification(&tx, &failed, Err(notify::Error::generic("lost")));

        assert!(failed.is_revoked());
        assert!(
            failed
                .take_error()
                .expect("backend error retained outside the queue")
                .contains("lost")
        );
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            WatchBackendNotification::Error {
                generation: 3,
                message
            } if message.contains("lost")
        ));
        let successor = WatchBackendLease::new(4, failed_generation, stop);
        assert!(!is_current_watch_generation(&successor, 3));
        assert!(is_current_watch_generation(&successor, 4));
        assert!(!successor.is_revoked());
    }

    #[test]
    fn typed_watch_interests_route_controls_public_assets_and_loader_inputs() {
        let root = PathBuf::from("project");
        let source = WatchInterest::tree(root.join("src"));
        for extension in ["mjs", "cjs", "mp4", "webm", "mp3", "wav", "ogg"] {
            assert!(source.matches(&root.join("src").join(format!("input.{extension}"))));
        }
        assert!(!source.matches(&root.join("src/readme.txt")));
        assert!(!WatchInterest::tree(&root).matches(&root.join(".wake/entry.tsx")));

        let public = WatchInterest::all_files_tree(root.join("public"));
        assert!(public.matches(&root.join("public/CNAME")));
        assert!(public.matches(&root.join("public/robots.txt")));

        let browsers = WatchInterest::exact_file(root.join(".browserslistrc"));
        assert!(browsers.matches(&root.join(".browserslistrc")));
        assert!(!browsers.matches(&root.join("nested/.browserslistrc")));
    }

    #[cfg(windows)]
    #[test]
    fn reported_watch_paths_coalesce_windows_verbatim_aliases() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source.js");
        std::fs::write(&source, "export default 1").unwrap();
        let canonical = std::fs::canonicalize(&source).unwrap();
        let normalized = wake_common::fs::normalize(&canonical);
        let verbatim = PathBuf::from(format!(r"\\?\{}", normalized.display()));
        let uppercase = PathBuf::from(normalized.to_string_lossy().to_uppercase());

        assert_eq!(
            reported_watch_paths(&[verbatim, uppercase, normalized.clone()]),
            vec![normalized]
        );
    }

    #[test]
    fn watch_invalidations_use_physical_identity_and_preserve_missing_suffixes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src/index.js");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "export default 1").unwrap();
        let missing = root.path().join("src/missing/dependency.js");

        let WatchInvalidation::Paths(paths) =
            WatchInvalidation::Paths(vec![source.clone(), missing.clone()]).normalized()
        else {
            unreachable!();
        };
        assert_eq!(paths[0], normalize_watch_path(&source));
        assert_eq!(paths[1], normalize_watch_path(&missing));
    }

    #[test]
    fn structural_events_match_descendants_and_missing_interest_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("missing/a/b");
        let tree = WatchInterest::tree(&nested).resolve_against(root.path());
        let exact =
            WatchInterest::exact_file(nested.join("wake.config.toml")).resolve_against(root.path());

        assert!(tree.matches_event(&root.path().join("missing"), true));
        assert!(exact.matches_event(&root.path().join("missing"), true));
        assert!(
            WatchInterest::tree(root.path().join("src"))
                .matches_event(&root.path().join("src/new-directory"), true)
        );
        assert_eq!(
            tree.registrations(),
            vec![(
                normalize_watch_path(root.path()),
                RecursiveMode::NonRecursive,
            )]
        );
    }

    #[test]
    fn exact_file_registration_stays_on_parent_across_replacements() {
        let root = tempfile::tempdir().unwrap();
        let control = root.path().join("wake.config.toml");
        std::fs::write(&control, "first").unwrap();
        let parent = normalize_watch_path(root.path());
        let interest = WatchInterest::exact_file(&control).resolve_against(root.path());
        let expected = vec![(parent, RecursiveMode::NonRecursive)];

        assert_eq!(interest.registrations(), expected);
        for contents in ["second", "third"] {
            std::fs::remove_file(&control).unwrap();
            std::fs::write(&control, contents).unwrap();
            assert_eq!(interest.registrations(), expected);
        }
    }

    #[test]
    fn recovery_rescan_fences_without_consuming_queued_lazy_load() {
        let (load_tx, load_rx) = mpsc::channel();
        let ticket = MountLoadTicket { index: 7, epoch: 3 };
        load_tx.send(ticket).unwrap();

        assert_eq!(next_mount_load(&load_rx, true), None);
        assert_eq!(next_mount_load(&load_rx, false), Some(ticket));
    }

    #[test]
    fn lazy_ticket_claim_is_exact_and_stale_or_duplicate_epochs_are_noops() {
        let (load_tx, _load_rx) = mpsc::channel();
        let loading = MountLoadingState::new(1, MountLoadPhase::Pending, load_tx);
        let MountReadiness::Enqueue(first) = loading.poll_readiness() else {
            panic!("pending mount must issue its first ticket");
        };
        assert_eq!(first, MountLoadTicket { index: 1, epoch: 1 });
        assert!(loading.claim(first));
        assert!(
            !loading.claim(first),
            "a duplicate ticket cannot claim Building"
        );
        assert!(loading.complete_attempt(first.epoch, MountAttemptCompletion::Retryable));

        let MountReadiness::Enqueue(second) = loading.poll_readiness() else {
            panic!("a recovered mount must issue a new epoch");
        };
        assert_eq!(second, MountLoadTicket { index: 1, epoch: 2 });
        assert!(
            !loading.claim(first),
            "the prior epoch cannot claim a new attempt"
        );
        assert!(!loading.claim(MountLoadTicket { index: 9, ..second }));
        assert!(loading.claim(second));
        assert!(!loading.complete_attempt(
            first.epoch,
            MountAttemptCompletion::Failed("stale failure".to_owned()),
        ));
        assert_eq!(loading.phase(), MountLoadPhase::Building(second.epoch));
        assert!(loading.complete_attempt(
            second.epoch,
            MountAttemptCompletion::Stopped("server stopped".to_owned()),
        ));
        loading.set_idle_phase(MountIdlePhase::Loaded);
        assert_eq!(
            loading.phase(),
            MountLoadPhase::Stopped("server stopped".to_owned()),
            "an unowned transition cannot overwrite the terminal stop"
        );
    }

    #[test]
    fn queued_ticket_survives_recovery_and_building_restarts_at_next_epoch() {
        let (load_tx, _load_rx) = mpsc::channel();
        let loading = MountLoadingState::new(2, MountLoadPhase::Pending, load_tx);
        let MountReadiness::Enqueue(first) = loading.poll_readiness() else {
            panic!("pending mount must queue");
        };

        assert!(!loading.recover_backend_loss());
        assert_eq!(loading.phase(), MountLoadPhase::Queued(first.epoch));
        assert!(!loading.set_idle_phase(MountIdlePhase::Pending));
        assert!(!loading.set_idle_phase(MountIdlePhase::Failed("rescan failed".to_owned())));
        assert_eq!(loading.phase(), MountLoadPhase::Queued(first.epoch));

        assert!(loading.claim(first));
        assert!(!loading.set_idle_phase(MountIdlePhase::Pending));
        assert!(loading.recover_backend_loss());
        assert_eq!(loading.phase(), MountLoadPhase::Pending);
        let MountReadiness::Enqueue(second) = loading.poll_readiness() else {
            panic!("recovered build must queue a replacement ticket");
        };
        assert_eq!(second.epoch, first.epoch + 1);
        assert!(!loading.claim(first));
    }

    #[test]
    fn lazy_epoch_overflow_is_terminal_instead_of_reusing_a_capability() {
        let (load_tx, load_rx) = mpsc::channel();
        let loading = MountLoadingState::new(1, MountLoadPhase::Pending, load_tx);
        loading.state.lock().unwrap().next_epoch = u64::MAX;

        assert!(matches!(
            loading.poll_readiness(),
            MountReadiness::Failed(_)
        ));
        assert!(matches!(loading.phase(), MountLoadPhase::Failed(_)));
        assert!(matches!(load_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }

    #[test]
    fn recovery_rescan_restores_eager_readiness_even_when_session_was_retained() {
        assert!(is_recovering_eager_mount(true, DevLoading::Eager));
        assert!(!is_recovering_eager_mount(false, DevLoading::Eager));
        assert!(!is_recovering_eager_mount(true, DevLoading::Lazy));
    }

    #[test]
    fn successful_normal_rebuild_restores_failed_mount_to_loaded() {
        let (load_tx, _load_rx) = mpsc::channel();
        let (federation_tx, _federation_rx) = broadcast::channel(1);
        let mount = Arc::new(MountedAppState {
            name: Some("workspace".to_owned()),
            base_path: "/workspace/".to_owned(),
            published: Arc::new(RwLock::new(PublishedMountGeneration::default())),
            public_dir: PathBuf::new(),
            federation_tx,
            loading: Arc::new(MountLoadingState::new(
                0,
                MountLoadPhase::Failed("old failure".to_owned()),
                load_tx,
            )),
        });
        let mounts = vec![Arc::clone(&mount)];

        mark_mount_rebuild_succeeded(&mounts, 0, None);

        assert_eq!(mount.loading.phase(), MountLoadPhase::Loaded);
    }

    #[test]
    fn readiness_subscription_observes_publish_between_check_and_await() {
        let (load_tx, _load_rx) = mpsc::channel();
        let loading = MountLoadingState::new(1, MountLoadPhase::Building(1), load_tx);
        let mut changed = loading.subscribe();
        assert_eq!(loading.poll_readiness(), MountReadiness::Wait);
        // Model the worker transition after the authoritative state check but before the handler
        // first polls `changed()`. The pre-read subscription must retain this unseen version.
        assert!(loading.complete_attempt(1, MountAttemptCompletion::Loaded));
        assert!(changed.has_changed().unwrap());

        assert!(
            matches!(changed.changed().now_or_never(), Some(Ok(()))),
            "the pre-read subscription must retain the unseen version"
        );
        changed.borrow_and_update();
        assert_eq!(loading.phase(), MountLoadPhase::Loaded);
    }

    #[test]
    fn thirty_two_waiters_issue_one_ticket_and_share_one_result() {
        let (load_tx, load_rx) = mpsc::channel();
        let loading = Arc::new(MountLoadingState::new(1, MountLoadPhase::Pending, load_tx));
        let stop = Arc::new(StopSignal::new());
        let mut waiters = (0..32)
            .map(|_| Box::pin(ensure_loading_ready(&loading, &stop)))
            .collect::<Vec<_>>();
        let waker = futures_util::task::noop_waker();
        let mut context = Context::from_waker(&waker);
        for waiter in &mut waiters {
            assert!(
                matches!(waiter.as_mut().poll(&mut context), Poll::Pending),
                "every waiter must be parked before the loader completes"
            );
        }

        let ticket = load_rx
            .try_recv()
            .expect("the first waiter must enqueue exactly one ticket");
        assert!(matches!(load_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        assert_eq!(loading.phase(), MountLoadPhase::Queued(ticket.epoch));
        assert!(loading.claim(ticket));
        assert_eq!(loading.phase(), MountLoadPhase::Building(ticket.epoch));
        assert!(loading.complete_attempt(ticket.epoch, MountAttemptCompletion::Loaded));

        let results = actix_web::rt::System::new()
            .block_on(async { futures_util::future::join_all(waiters).await });
        assert!(results.into_iter().all(|result| result.is_ok()));
    }

    #[test]
    fn loader_disconnect_is_terminal_and_wakes_readiness() {
        let (load_tx, load_rx) = mpsc::channel();
        drop(load_rx);
        let loading = MountLoadingState::new(1, MountLoadPhase::Pending, load_tx);
        let stop = StopSignal::new();

        let error = actix_web::rt::System::new()
            .block_on(ensure_loading_ready(&loading, &stop))
            .unwrap_err();
        assert!(error.contains("loader stopped"));
        assert!(matches!(loading.phase(), MountLoadPhase::Stopped(_)));
        assert!(!loading.set_idle_phase(MountIdlePhase::Pending));
        assert!(!loading.set_idle_phase(MountIdlePhase::Failed("retry".to_owned())));
        assert!(matches!(loading.phase(), MountLoadPhase::Stopped(_)));
    }

    #[test]
    fn shutdown_wakes_an_async_lazy_waiter() {
        let (load_tx, load_rx) = mpsc::channel();
        let loading = Arc::new(MountLoadingState::new(1, MountLoadPhase::Pending, load_tx));
        let stop = Arc::new(StopSignal::new());
        let waiter_loading = Arc::clone(&loading);
        let waiter_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result = actix_web::rt::System::new()
                .block_on(ensure_loading_ready(&waiter_loading, &waiter_stop));
            done_tx.send(result).unwrap();
        });
        load_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.request();

        let error = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stop must wake the async waiter")
            .unwrap_err();
        assert!(error.contains("stopping"));
        waiter.join().unwrap();
    }

    #[test]
    fn worker_exit_unblocks_a_queued_lazy_waiter() {
        let (load_tx, load_rx) = mpsc::channel();
        let loading = Arc::new(MountLoadingState::new(1, MountLoadPhase::Pending, load_tx));
        let finalizer = MountWaiterFinalizer {
            loading: vec![Arc::clone(&loading)],
        };
        let stop = Arc::new(StopSignal::new());
        let waiter_stop = Arc::clone(&stop);
        let (done_tx, done_rx) = mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let result =
                actix_web::rt::System::new().block_on(ensure_loading_ready(&loading, &waiter_stop));
            done_tx.send(result).unwrap();
        });
        assert_eq!(
            load_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            MountLoadTicket { index: 1, epoch: 1 }
        );

        drop(finalizer);
        let error = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("worker finalization must wake the async waiter")
            .unwrap_err();
        assert!(error.contains("worker stopped"));
        waiter.join().unwrap();
    }

    #[derive(Default)]
    struct FakeWatcher {
        actual: BTreeMap<PathBuf, RecursiveMode>,
        fail_watch_once: BTreeSet<PathBuf>,
        fail_unwatch_once: BTreeSet<PathBuf>,
        operations: Vec<(bool, PathBuf, RecursiveMode)>,
    }

    impl Watcher for FakeWatcher {
        fn new<F: notify::EventHandler>(
            _event_handler: F,
            _config: notify::Config,
        ) -> notify::Result<Self> {
            Ok(Self::default())
        }

        fn watch(&mut self, path: &Path, mode: RecursiveMode) -> notify::Result<()> {
            if self.fail_watch_once.remove(path) {
                return Err(notify::Error::generic("injected watch failure"));
            }
            self.actual.insert(path.to_path_buf(), mode);
            self.operations.push((true, path.to_path_buf(), mode));
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            if self.fail_unwatch_once.remove(path) {
                return Err(notify::Error::generic("injected unwatch failure"));
            }
            let mode = self
                .actual
                .remove(path)
                .ok_or_else(notify::Error::watch_not_found)?;
            self.operations.push((false, path.to_path_buf(), mode));
            Ok(())
        }

        fn kind() -> notify::WatcherKind {
            notify::WatcherKind::NullWatcher
        }
    }

    #[test]
    fn watch_registration_state_is_truthful_across_failure_retry_and_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let first = normalize_watch_path(&first);
        let second = normalize_watch_path(&second);
        let first_tree = WatchInterest::tree(&first).resolve_against(root.path());
        let second_tree = WatchInterest::tree(&second).resolve_against(root.path());
        let mut watcher = FakeWatcher::default();
        let mut state = WatchRegistrationState::default();

        reconcile_watch_interests(&mut watcher, &mut state, std::slice::from_ref(&first_tree))
            .unwrap();
        assert_eq!(watcher.actual.get(&first), Some(&RecursiveMode::Recursive));

        // Recursive coverage is stronger than the same backend path requested non-recursively;
        // reconciliation must not create a lossy downgrade window.
        let exact_child =
            WatchInterest::exact_file(first.join("missing.js")).resolve_against(root.path());
        watcher.operations.clear();
        reconcile_watch_interests(&mut watcher, &mut state, std::slice::from_ref(&exact_child))
            .unwrap();
        assert!(
            watcher
                .operations
                .iter()
                .all(|(added, _, mode)| !*added && *mode == RecursiveMode::Recursive)
        );
        assert!(
            watcher
                .actual
                .values()
                .all(|mode| *mode == RecursiveMode::Recursive)
        );
        assert!(state.is_coverage_complete(std::slice::from_ref(&exact_child)));

        watcher.fail_watch_once.insert(second.clone());
        assert!(
            reconcile_watch_interests(
                &mut watcher,
                &mut state,
                &[first_tree.clone(), second_tree.clone()],
            )
            .is_err()
        );
        assert!(!state.is_coverage_complete(&[first_tree.clone(), second_tree.clone()]));
        assert!(!watcher.actual.contains_key(&second));

        let retry = reconcile_watch_interests(
            &mut watcher,
            &mut state,
            &[first_tree.clone(), second_tree.clone()],
        )
        .unwrap();
        assert!(retry.coverage_changed);
        assert!(state.is_coverage_complete(&[first_tree.clone(), second_tree.clone()]));

        watcher.fail_unwatch_once.insert(second.clone());
        let cleanup =
            reconcile_watch_interests(&mut watcher, &mut state, std::slice::from_ref(&first_tree))
                .unwrap();
        assert_eq!(cleanup.cleanup_errors.len(), 1);
        assert!(state.is_coverage_complete(std::slice::from_ref(&first_tree)));
        assert!(!state.is_converged(std::slice::from_ref(&first_tree)));
        reconcile_watch_interests(&mut watcher, &mut state, std::slice::from_ref(&first_tree))
            .unwrap();
        assert!(!watcher.actual.contains_key(&second));
        assert!(state.is_converged(std::slice::from_ref(&first_tree)));
    }

    #[test]
    fn missing_tree_promotion_adds_recursive_coverage_before_removing_parent() {
        let root = tempfile::tempdir().unwrap();
        let declared = root.path().join("missing");
        let interest = WatchInterest::tree(&declared).resolve_against(root.path());
        let root_identity = normalize_watch_path(root.path());
        let mut watcher = FakeWatcher::default();
        let mut state = WatchRegistrationState::default();
        reconcile_watch_interests(&mut watcher, &mut state, &[interest]).unwrap();
        assert_eq!(
            watcher.actual.get(&root_identity),
            Some(&RecursiveMode::NonRecursive)
        );

        std::fs::create_dir(&declared).unwrap();
        let promoted = WatchInterest::tree(&declared).resolve_against(root.path());
        let promoted_identity = normalize_watch_path(&declared);
        watcher.operations.clear();
        reconcile_watch_interests(&mut watcher, &mut state, &[promoted]).unwrap();

        let removed_parent = watcher
            .operations
            .iter()
            .position(|operation| {
                *operation == (false, root_identity.clone(), RecursiveMode::NonRecursive)
            })
            .expect("parent cleanup");
        assert!(
            watcher.operations[..removed_parent]
                .iter()
                .all(|operation| { operation.0 && operation.2 == RecursiveMode::Recursive })
        );
        assert!(
            watcher.operations[..removed_parent]
                .iter()
                .any(|operation| operation.1 == promoted_identity)
        );
        assert_eq!(
            watcher.operations[removed_parent],
            (false, root_identity, RecursiveMode::NonRecursive)
        );
    }

    #[test]
    fn source_tree_rejects_owned_outputs_but_not_similarly_named_sources() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().to_path_buf();
        let interest = WatchInterest::tree(&root)
            .excluding_tree(root.join("dist"))
            .resolve_against(&root);
        let all_files = WatchInterest::all_files_tree(&root).resolve_against(&root);

        assert!(!interest.matches(&root.join("dist/chunk.js")));
        assert!(!interest.matches(&root.join(".wake-output-stage-7/chunk.js")));
        assert!(!interest.matches(&root.join(".wake/generated.js")));
        assert!(interest.matches(&root.join(".wakeful/source.js")));
        assert!(interest.matches(&root.join("src/index.js")));
        assert!(!all_files.matches(&root.join(".wake-docs-next-7/CNAME")));
        assert!(!all_files.matches(&root.join(".wake-app-backup-7/CNAME")));
        assert!(all_files.matches(&root.join(".wake/generated.js")));
        assert!(all_files.matches(&root.join(".wakeful/CNAME")));
    }

    #[test]
    fn resolved_tree_symlink_also_watches_its_declared_parent() {
        let fixture = tempfile::tempdir().unwrap();
        let declared = fixture.path().join("linked");
        let target = fixture.path().join("target");
        std::fs::create_dir(&target).unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&target, &declared);
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_dir(&target, &declared);
        if linked.is_err() {
            // Creating symlinks can require an OS capability unavailable to Windows CI users.
            return;
        }

        let interest = WatchInterest::tree(&declared).resolve_against(fixture.path());
        let registrations = interest.registrations();
        assert!(registrations.iter().any(|(path, mode)| {
            paths_equal(path, &normalize_watch_path(&target)) && *mode == RecursiveMode::Recursive
        }));
        assert!(registrations.iter().any(|(path, mode)| {
            paths_equal(path, &normalize_watch_path(fixture.path()))
                && *mode == RecursiveMode::NonRecursive
        }));
    }

    #[test]
    fn mount_watch_plan_unions_default_source_entry_public_and_extra_roots() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(root.path().join("extra")).unwrap();
        let entry = root.path().join("entry.tsx");
        std::fs::write(&entry, "export default 1").unwrap();
        let spec = MountSpec {
            name: None,
            root: root.path().to_path_buf(),
            base_path: "/".to_owned(),
            loading: DevLoading::Eager,
            plan: Some(DevMountPlan {
                entry: entry.clone(),
                file_system: Arc::new(OsFileSystem),
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_owned(),
            }),
            watch_interests: vec![WatchInterest::tree(root.path().join("extra"))],
            refresh: None,
            deferred_refresh: None,
            federation: FederationBuildOptions::default(),
            federation_updates_url: String::new(),
        };
        let interests = mount_watch_interests(&spec);
        for path in [
            root.path().join("src/app.tsx"),
            root.path().join("extra/component.tsx"),
            entry,
            root.path().join("public/CNAME"),
            root.path().join("index.html"),
        ] {
            assert!(
                interests.iter().any(|interest| interest.matches(&path)),
                "missing watch interest for {}",
                path.display()
            );
        }
    }

    fn http_request_with_headers(port: u16, method: &str, path: &str, headers: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .write_all(
                format!(
                    "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{headers}Connection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    fn http_request(port: u16, method: &str, path: &str) -> String {
        http_request_with_headers(port, method, path, "")
    }

    fn http_get(port: u16, path: &str) -> String {
        http_request(port, "GET", path)
    }

    fn test_federation_options(container_name: &str) -> FederationBuildOptions {
        FederationBuildOptions {
            enabled: true,
            container_name: container_name.to_owned(),
            browser_target: "chromium>=120".to_owned(),
            remote_entry_template: Some(format!(
                "export const wakeDevBuildId={};",
                serde_json::to_string(FEDERATION_BUILD_ID_PLACEHOLDER).unwrap()
            )),
            ..FederationBuildOptions::default()
        }
    }

    fn publication_snapshot(
        bundle: &str,
        previous: Option<&federation::FederationSnapshot>,
    ) -> federation::FederationSnapshot {
        let output = BuildOutput {
            bundle: bundle.to_owned(),
            module_count: 1,
            updated_module_count: 1,
            cached_module_count: 0,
            diagnostics: Vec::new(),
            chunks: vec![OutputChunk {
                name: "container".to_owned(),
                file_name: "container.js".to_owned(),
                code: bundle.to_owned(),
                kind: ChunkKind::Initial,
                is_entry: true,
                chunk_id: 0,
                module_ids: vec![0],
                imports: Vec::new(),
                dynamic_imports: Vec::new(),
                styles: Vec::new(),
                source_map: None,
            }],
            entry_chunk: 0,
            assets: Vec::new(),
        };
        federation::FederationSnapshot::assemble(
            &FederationBuildOptions {
                enabled: true,
                container_name: "catalog".to_owned(),
                browser_target: "chromium>=120".to_owned(),
                remote_entry_template: Some(format!(
                    "export const buildId={};",
                    serde_json::to_string(FEDERATION_BUILD_ID_PLACEHOLDER).unwrap()
                )),
                ..FederationBuildOptions::default()
            },
            &output,
            Arc::new(OsFileSystem),
            "/",
            "ws://localhost/__wake_federation_updates?remote=catalog",
            previous,
        )
        .unwrap()
        .0
    }

    #[test]
    fn published_mount_generation_never_exposes_crossed_bundle_and_manifest() {
        let first_snapshot = publication_snapshot("bundle-first", None);
        let second_snapshot = publication_snapshot("bundle-second", Some(&first_snapshot));
        let first_build_id = first_snapshot.manifest.build_id.as_str().to_owned();
        let second_build_id = second_snapshot.manifest.build_id.as_str().to_owned();
        assert_ne!(first_build_id, second_build_id);

        let published = Arc::new(RwLock::new(PublishedMountGeneration::default()));
        let _ = published.write().unwrap().install(PublishedMountCandidate {
            bundle: BundleState {
                js: "bundle-first".to_owned(),
                ..BundleState::default()
            },
            html: "first".to_owned(),
            federation: Some((first_snapshot.clone(), None)),
        });

        let barrier = Arc::new(Barrier::new(2));
        let done = Arc::new(AtomicBool::new(false));
        let writer_state = Arc::clone(&published);
        let writer_barrier = Arc::clone(&barrier);
        let writer_done = Arc::clone(&done);
        let writer = std::thread::spawn(move || {
            writer_barrier.wait();
            for generation in 0..2_000 {
                let (bundle, snapshot) = if generation % 2 == 0 {
                    ("bundle-second", second_snapshot.clone())
                } else {
                    ("bundle-first", first_snapshot.clone())
                };
                let _ = writer_state
                    .write()
                    .unwrap()
                    .install(PublishedMountCandidate {
                        bundle: BundleState {
                            js: bundle.to_owned(),
                            ..BundleState::default()
                        },
                        html: bundle.to_owned(),
                        federation: Some((snapshot, None)),
                    });
            }
            writer_done.store(true, Ordering::Release);
        });

        barrier.wait();
        let mut observations = 0;
        while !done.load(Ordering::Acquire) || observations == 0 {
            let generation = published.read().unwrap();
            let build_id = generation
                .federation
                .current()
                .expect("installed Federation snapshot")
                .manifest
                .build_id
                .as_str();
            match generation.bundle.js.as_str() {
                "bundle-first" => assert_eq!(build_id, first_build_id),
                "bundle-second" => assert_eq!(build_id, second_build_id),
                bundle => panic!("unexpected bundle `{bundle}`"),
            }
            observations += 1;
        }
        writer.join().unwrap();
        assert!(observations > 0);
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
    fn federation_update_uses_the_versioned_contract_shape() {
        let mut update = DevUpdate::new(
            "catalog".into(),
            Some("build-old".into()),
            "build-new".into(),
            7,
            DevUpdateAction::IsolatedRemount,
        );
        update.changed_exposes = vec!["./Z".into(), "./Button".into(), "./Z".into()];
        update.types_hash = Some("types-2".to_string());
        let message = msg_federation_update(&update.normalized()).unwrap();

        assert_eq!(
            message,
            r#"{"schemaVersion":"wake.federation.dev-update.v1","remote":"catalog","oldBuildId":"build-old","newBuildId":"build-new","changedExposes":["./Button","./Z"],"typesHash":"types-2","generation":7,"action":"isolated-remount"}"#
        );
    }

    #[test]
    fn dedicated_federation_transport_rejects_non_protocol_frames() {
        let valid = msg_federation_update(&DevUpdate::new(
            "catalog".into(),
            Some("old".into()),
            "new".into(),
            2,
            DevUpdateAction::FullReload,
        ))
        .unwrap();
        assert!(accepts_federation_update(&valid, "catalog"));
        assert!(!accepts_federation_update(&valid, "checkout"));
        assert!(!accepts_federation_update(
            r#"{"type":"reload","mount":"catalog"}"#,
            "catalog"
        ));
        assert!(!accepts_federation_update("not-json", "catalog"));
    }

    #[test]
    fn federation_rebuilds_use_separate_remote_and_local_update_semantics() {
        for (action, expected_local_type) in [
            (DevUpdateAction::TypesOnly, None),
            (
                DevUpdateAction::IsolatedRemount,
                Some("wake.federation.dev-update.v1"),
            ),
            (DevUpdateAction::FullReload, Some("reload")),
        ] {
            let (federation_tx, mut federation_rx) = broadcast::channel(4);
            let (local_tx, mut local_rx) = broadcast::channel(4);
            let update = DevUpdate::new(
                "catalog".into(),
                Some("old".into()),
                "new".into(),
                2,
                action,
            );
            publish_federation_build_update(
                &update,
                Some("catalog-preview"),
                &federation_tx,
                &local_tx,
                None,
            );

            let remote: serde_json::Value =
                serde_json::from_str(&federation_rx.try_recv().unwrap()).unwrap();
            assert_eq!(remote["schemaVersion"], "wake.federation.dev-update.v1");
            match expected_local_type {
                Some(expected) => {
                    let local: serde_json::Value =
                        serde_json::from_str(&local_rx.try_recv().unwrap()).unwrap();
                    let observed = local
                        .get("schemaVersion")
                        .or_else(|| local.get("type"))
                        .and_then(serde_json::Value::as_str);
                    assert_eq!(observed, Some(expected));
                }
                None => assert!(matches!(
                    local_rx.try_recv(),
                    Err(broadcast::error::TryRecvError::Empty)
                )),
            }
        }
    }

    #[test]
    fn browser_client_routes_federation_actions_through_the_window_broker() {
        let runtime = client_runtime();
        assert!(runtime.contains(r#"Symbol.for("wake.federation.v1")"#));
        assert!(runtime.contains("broker.applyDevUpdate(message)"));
        assert!(runtime.contains(r#"message.action === "types-only""#));
        assert!(runtime.contains(r#"update.action === "full-reload""#));
        assert!(runtime.contains(r#"wake:federation:isolated-remount"#));
        let invalidate = runtime.find("broker.applyDevUpdate(message)").unwrap();
        let dispatch = runtime
            .find(r#"window.dispatchEvent(new CustomEvent("wake:federation:isolated-remount""#)
            .unwrap();
        assert!(invalidate < dispatch);
    }

    #[test]
    fn ordinary_browser_updates_are_a_closed_live_reload_contract() {
        assert_eq!(msg_ready(), r#"{"type":"ready"}"#);
        assert_eq!(
            msg_reload(Some("docs")),
            r#"{"type":"reload","mount":"docs"}"#
        );
        assert_eq!(
            msg_error("bad \"source\"\nline", None),
            r#"{"type":"error","message":"bad \"source\"\nline","mount":""}"#
        );

        let runtime = client_runtime();
        assert!(runtime.contains(LIVE_RELOAD_ENDPOINT));
        assert!(!runtime.contains("/__wake_hmr"));
        assert!(runtime.contains(r#"m.type === "reload""#));
        assert!(runtime.contains("location.reload()"));
        assert!(!runtime.contains("import.meta.hot"));
    }

    #[test]
    fn federation_build_options_reach_the_single_mount_session() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-options-")
            .tempdir()
            .unwrap();
        let source_dir = root.path().join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let entry = source_dir.join("index.js");
        std::fs::write(
            &entry,
            "import React from 'react';export async function load(){return [React.version,await import('catalog/Button')]} ",
        )
        .unwrap();
        let spec = MountSpec {
            name: None,
            root: root.path().to_path_buf(),
            base_path: "/".to_string(),
            loading: DevLoading::Eager,
            plan: Some(DevMountPlan {
                entry: entry.clone(),
                file_system: Arc::new(OsFileSystem),
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_string(),
            }),
            watch_interests: Vec::new(),
            refresh: None,
            deferred_refresh: None,
            federation: FederationBuildOptions {
                enabled: true,
                container_name: "shell".to_string(),
                remotes: vec!["catalog".to_string()],
                shared: vec![(
                    "react".to_string(),
                    "react".to_string(),
                    "react18".to_string(),
                )],
                entry_export: Some("./App".to_string()),
                bootstrap: None,
                ..FederationBuildOptions::default()
            },
            federation_updates_url: "ws://localhost/__wake_federation_updates?remote=shell"
                .to_owned(),
        };

        let mut session = create_mount_session(&spec);
        let (output, _) = session.build_current_generation(BuildRequest::new(&entry));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert!(
            output
                .bundle
                .contains("__wake_require__.runtimeImport(\"catalog/Button\")"),
            "{}",
            output.bundle
        );
        assert!(
            output
                .bundle
                .contains("__wake_require__.shared(\"react\", \"react18\")"),
            "{}",
            output.bundle
        );
        assert!(output.bundle.contains("wake.federation.exposes.v1"));
    }

    #[test]
    fn host_only_synthetic_container_publishes_its_application_loader() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-host-container-")
            .tempdir()
            .unwrap();
        let source_dir = root.path().join("src");
        std::fs::create_dir_all(&source_dir).unwrap();
        let entry = source_dir.join("container.js");
        std::fs::write(
            &entry,
            "export const __wakeApp=()=>Promise.resolve({started:true});",
        )
        .unwrap();
        let spec = MountSpec {
            name: None,
            root: root.path().to_path_buf(),
            base_path: "/".to_owned(),
            loading: DevLoading::Eager,
            plan: Some(DevMountPlan {
                entry: entry.clone(),
                file_system: Arc::new(OsFileSystem),
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_owned(),
            }),
            watch_interests: Vec::new(),
            refresh: None,
            deferred_refresh: None,
            federation: FederationBuildOptions {
                enabled: true,
                container_name: "shell".to_owned(),
                application_loader_export: Some("__wakeApp".to_owned()),
                ..FederationBuildOptions::default()
            },
            federation_updates_url: "ws://localhost/__wake_federation_updates?remote=shell"
                .to_owned(),
        };

        let mut session = create_mount_session(&spec);
        let (output, _) = session.build_current_generation(BuildRequest::new(&entry));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert!(output.bundle.contains("wake.federation.exposes.v1"));
        assert!(output.bundle.contains("./__wake_container__"));
        assert!(output.bundle.contains("__wakeApp"));
    }

    #[test]
    fn federation_runtime_and_types_share_one_mount_generation_snapshot() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-generation-")
            .tempdir()
            .unwrap();
        let entry = root.path().join("src/index.js");
        let first_source = "export const generation = 'dev-snapshot-v1';";
        let later_source = "export const generation = 'dev-snapshot-v2';";
        let source_file_system = Arc::new(AdvancingSourceFileSystem::new(
            entry.clone(),
            first_source,
            later_source,
        ));
        let spec = MountSpec {
            name: None,
            root: root.path().to_path_buf(),
            base_path: "/".to_owned(),
            loading: DevLoading::Eager,
            plan: Some(DevMountPlan {
                entry: entry.clone(),
                file_system: source_file_system.clone(),
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_owned(),
            }),
            watch_interests: Vec::new(),
            refresh: None,
            deferred_refresh: None,
            federation: FederationBuildOptions::default(),
            federation_updates_url: "ws://localhost/__wake_federation_updates?remote=catalog"
                .to_owned(),
        };

        let observed_type_sources = Arc::new(Mutex::new(Vec::new()));
        let mut federation = test_federation_options("catalog");
        federation.type_emitter = Some(FederationTypeEmitter::new({
            let entry = entry.clone();
            let observed_type_sources = Arc::clone(&observed_type_sources);
            move |file_system| {
                let source = file_system
                    .read_to_string(&entry)
                    .map_err(|error| error.to_string())?;
                observed_type_sources.lock().unwrap().push(source.clone());
                Ok(FederationTypeGeneration::new(
                    source.as_bytes().to_vec(),
                    move |build_id| {
                        Ok(FederationTypeOutput {
                            bundle_json: serde_json::to_vec(&serde_json::json!({
                                "schemaVersion": "wake.federation.types.v1",
                                "name": "catalog",
                                "buildId": build_id.as_str(),
                                "exposes": {},
                                "modules": {"catalog/generation": source},
                            }))
                            .unwrap(),
                            ambient_declaration: "declare module \"catalog/generation\" {}\n"
                                .to_owned(),
                        })
                    },
                ))
            }
        }));

        let mut session = create_mount_session(&spec);
        let (first_output, first_file_system) =
            session.build_current_generation(BuildRequest::new(&entry));
        assert!(first_output.bundle.contains("dev-snapshot-v1"));
        let first_diagnostic_file_system = Arc::clone(&first_file_system);
        let (first_snapshot, _) = federation::FederationSnapshot::assemble(
            &federation,
            first_output,
            first_file_system,
            "/",
            &spec.federation_updates_url,
            None,
        )
        .unwrap();
        assert_eq!(
            capture_diagnostic_sources(
                &[Diagnostic::error("generation probe")
                    .with_path(entry.to_string_lossy().into_owned())],
                first_diagnostic_file_system.as_ref(),
            ),
            vec![DiagnosticSource {
                path: entry.clone(),
                text: first_source.to_owned(),
            }]
        );
        assert_eq!(source_file_system.source_read_count(), 1);
        assert_eq!(
            *observed_type_sources.lock().unwrap(),
            vec![first_source.to_owned()]
        );

        session.invalidate_paths(std::slice::from_ref(&entry), false);
        assert_eq!(session.generation.generation(), 1);
        assert_eq!(session.session.generation(), 1);
        let (second_output, second_file_system) =
            session.build_current_generation(BuildRequest::new(&entry));
        assert!(
            second_output.bundle.contains("dev-snapshot-v2"),
            "source reads: {}; bundle: {}",
            source_file_system.source_read_count(),
            second_output.bundle
        );
        let (second_snapshot, _) = federation::FederationSnapshot::assemble(
            &federation,
            second_output,
            second_file_system,
            "/",
            &spec.federation_updates_url,
            Some(&first_snapshot),
        )
        .unwrap();

        assert_ne!(
            first_snapshot.manifest.build_id,
            second_snapshot.manifest.build_id
        );
        assert_eq!(source_file_system.source_read_count(), 2);
        assert_eq!(
            *observed_type_sources.lock().unwrap(),
            vec![first_source.to_owned(), later_source.to_owned()]
        );
    }

    #[test]
    fn javascript_sourcemap_header_keeps_the_body_unchanged() {
        let code = "globalThis.answer = 42;";
        let response = javascript_response(code.to_string(), Some("/async.123.js.map"));

        assert_eq!(
            response
                .headers()
                .get("SourceMap")
                .and_then(|value| value.to_str().ok()),
            Some("/async.123.js.map")
        );
        let body = actix_web::rt::System::new()
            .block_on(actix_web::body::to_bytes(response.into_body()))
            .unwrap();
        assert_eq!(body.as_ref(), code.as_bytes());
        assert!(
            !body
                .windows(b"sourceMappingURL".len())
                .any(|window| { window == b"sourceMappingURL" })
        );
    }

    #[test]
    fn federation_lease_frames_are_canonical_bounded_and_remote_scoped() {
        assert!(!invalid_federation_non_text_frame(
            &actix_ws::Message::Pong(web::Bytes::new())
        ));
        assert!(!invalid_federation_non_text_frame(&actix_ws::Message::Nop));
        assert!(invalid_federation_non_text_frame(
            &actix_ws::Message::Binary(web::Bytes::new())
        ));
        let valid = serde_json::to_string(&DevLeaseMessage::lease(
            "catalog".into(),
            vec!["build-a".into(), "build-b".into()],
        ))
        .unwrap();
        let (build_ids, canonical) = decode_federation_lease(&valid, "catalog").unwrap();
        assert_eq!(build_ids, vec!["build-a".into(), "build-b".into()]);
        assert_eq!(canonical, build_ids.into_iter().collect());

        assert_eq!(
            decode_federation_lease(&valid, "checkout"),
            Err(DevLeaseReloadReason::InvalidLease)
        );
        let duplicate = serde_json::to_string(&DevLeaseMessage::lease(
            "catalog".into(),
            vec!["build-a".into(), "build-a".into()],
        ))
        .unwrap();
        assert_eq!(
            decode_federation_lease(&duplicate, "catalog"),
            Err(DevLeaseReloadReason::InvalidLease)
        );
        let over_limit = serde_json::to_string(&DevLeaseMessage::lease(
            "catalog".into(),
            (0..=FEDERATION_DEV_MAX_BUILD_LEASES)
                .map(|index| format!("build-{index:02}").into())
                .collect(),
        ))
        .unwrap();
        assert_eq!(
            decode_federation_lease(&over_limit, "catalog"),
            Err(DevLeaseReloadReason::LeaseLimit)
        );
        let server_only = serde_json::to_string(&DevLeaseMessage::lease_ack(
            "catalog".into(),
            vec!["build-a".into()],
            "build-a".into(),
            1,
        ))
        .unwrap();
        assert_eq!(
            decode_federation_lease(&server_only, "catalog"),
            Err(DevLeaseReloadReason::InvalidLease)
        );
    }

    #[test]
    fn federation_gone_head_exposes_the_typed_cross_origin_reload_control() {
        let cursor = federation::FederationSnapshotCursor {
            remote: "catalog".into(),
            current_build_id: "current".into(),
            generation: 9,
        };
        let get = federation_gone_response(cursor.clone(), "expired".into(), false);
        assert_eq!(get.status(), actix_web::http::StatusCode::GONE);
        for (header, expected) in [
            (
                FEDERATION_CONTROL_HEADER,
                FEDERATION_DEV_LEASE_SCHEMA_VERSION,
            ),
            (FEDERATION_ACTION_HEADER, "full-reload"),
            (FEDERATION_REMOTE_HEADER, "catalog"),
            (FEDERATION_CURRENT_BUILD_HEADER, "current"),
            (FEDERATION_GENERATION_HEADER, "9"),
            (FEDERATION_EXPIRED_BUILD_HEADER, "expired"),
            (FEDERATION_REASON_HEADER, "build-gone"),
            ("Access-Control-Allow-Origin", "*"),
            (
                "Access-Control-Expose-Headers",
                FEDERATION_CONTROL_EXPOSE_HEADERS,
            ),
        ] {
            assert_eq!(
                get.headers()
                    .get(header)
                    .and_then(|value| value.to_str().ok()),
                Some(expected),
                "header {header}"
            );
        }
        let representation_length = get.headers().get("Content-Length").unwrap().clone();
        let body = actix_web::rt::System::new()
            .block_on(actix_web::body::to_bytes(get.into_body()))
            .unwrap();
        let decoded: DevLeaseMessage = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            decoded,
            DevLeaseMessage::full_reload(
                "catalog".into(),
                "current".into(),
                9,
                Some("expired".into()),
                DevLeaseReloadReason::BuildGone,
            )
        );

        let head = federation_gone_response(cursor, "expired".into(), true);
        assert_eq!(
            head.headers().get("Content-Length").unwrap(),
            representation_length
        );
        assert_eq!(
            head.headers()
                .get("Access-Control-Expose-Headers")
                .unwrap()
                .to_str()
                .unwrap(),
            FEDERATION_CONTROL_EXPOSE_HEADERS
        );
        let body = actix_web::rt::System::new()
            .block_on(actix_web::body::to_bytes(head.into_body()))
            .unwrap();
        assert!(body.is_empty());
    }

    #[test]
    fn federation_head_matches_get_headers_without_downloading_the_body() {
        let route = federation::FederationRoute {
            bytes: b"export const answer = 42;".to_vec(),
            mime: "text/javascript".to_owned(),
            source_map_url: Some("/@wake/build/answer.js.map".to_owned()),
        };
        let get = federation_route_response(route.clone(), false, false);
        assert_eq!(
            get.headers()
                .get("Content-Type")
                .and_then(|value| value.to_str().ok()),
            Some("text/javascript")
        );
        assert_eq!(
            get.headers()
                .get("Content-Length")
                .and_then(|value| value.to_str().ok()),
            Some("25")
        );
        assert_eq!(
            get.headers()
                .get("Access-Control-Allow-Origin")
                .and_then(|value| value.to_str().ok()),
            Some("*")
        );
        assert_eq!(
            get.headers()
                .get("Cache-Control")
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );

        let head = federation_route_response(route, false, true);
        assert_eq!(
            head.headers()
                .get("Content-Length")
                .and_then(|value| value.to_str().ok()),
            Some("25")
        );
        let body = actix_web::rt::System::new()
            .block_on(actix_web::body::to_bytes(head.into_body()))
            .unwrap();
        assert!(body.is_empty());

        let manifest = federation_route_response(
            federation::FederationRoute {
                bytes: b"{}".to_vec(),
                mime: "application/json".to_owned(),
                source_map_url: None,
            },
            true,
            false,
        );
        assert_eq!(
            manifest
                .headers()
                .get("Cache-Control")
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
    }

    #[test]
    fn duplicate_enabled_federation_container_names_are_rejected_across_mounts() {
        let root = tempfile::Builder::new()
            .prefix("wake-dev-duplicate-federation-site-")
            .tempdir()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("wake-dev-duplicate-federation-mount-")
            .tempdir()
            .unwrap();
        for directory in [root.path(), workspace.path()] {
            std::fs::create_dir_all(directory.join("src")).unwrap();
            std::fs::write(directory.join("src/index.js"), "export const ready = true;").unwrap();
        }
        let result = start(
            root.path(),
            0,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                federation: test_federation_options("catalog"),
                mounts: vec![MountedServeOptions {
                    name: "workspace".to_owned(),
                    root: workspace.path().to_path_buf(),
                    base_path: "/workspace/".to_owned(),
                    loading: DevLoading::Eager,
                    entry: workspace.path().join("src/index.js"),
                    file_system: None,
                    resolve_options: ResolveOptions::default(),
                    define: Vec::new(),
                    target_env: TargetEnv::default(),
                    jsx_import_source: "react".to_owned(),
                    watch_interests: vec![WatchInterest::tree(workspace.path().join("src"))],
                    refresh: None,
                    federation: test_federation_options("catalog"),
                }],
                ..ServeOptions::default()
            },
        );
        let error = match result {
            Ok(server) => {
                server.close().unwrap();
                panic!("duplicate Federation containers unexpectedly started")
            }
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("duplicate enabled Federation container name `catalog`"),
            "{error}"
        );
    }

    #[test]
    fn sibling_remote_broadcast_lag_cannot_refresh_or_starve_this_socket() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-channel-site-")
            .tempdir()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("wake-dev-federation-channel-mount-")
            .tempdir()
            .unwrap();
        for directory in [root.path(), workspace.path()] {
            std::fs::create_dir_all(directory.join("src")).unwrap();
            std::fs::write(directory.join("src/index.js"), "export const ready = true;").unwrap();
        }
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                federation: test_federation_options("alpha"),
                mounts: vec![MountedServeOptions {
                    name: "beta-workspace".to_owned(),
                    root: workspace.path().to_path_buf(),
                    base_path: "/beta/".to_owned(),
                    loading: DevLoading::Eager,
                    entry: workspace.path().join("src/index.js"),
                    file_system: None,
                    resolve_options: ResolveOptions::default(),
                    define: Vec::new(),
                    target_env: TargetEnv::default(),
                    jsx_import_source: "react".to_owned(),
                    watch_interests: vec![WatchInterest::tree(workspace.path().join("src"))],
                    refresh: None,
                    federation: test_federation_options("beta"),
                }],
                ..ServeOptions::default()
            },
        )
        .unwrap();
        let manifest_response = http_get(port, "/wake-federation.json");
        let (_, manifest_body) = manifest_response.split_once("\r\n\r\n").unwrap();
        let manifest: wake_federation_contract::Manifest =
            serde_json::from_str(manifest_body).unwrap();
        let mut beta_rx = server
            .inner
            .federation_senders
            .get("beta")
            .unwrap()
            .subscribe();

        actix_web::rt::System::new().block_on(async {
            let url = format!("ws://127.0.0.1:{port}/__wake_federation_updates?remote=alpha");
            let (_, mut socket) = awc::Client::new().ws(url).connect().await.unwrap();
            let lease = serde_json::to_string(&DevLeaseMessage::lease(
                "alpha".into(),
                vec![manifest.build_id.clone()],
            ))
            .unwrap();
            socket
                .send(awc::ws::Message::Text(lease.into()))
                .await
                .unwrap();
            let ack = socket.next().await.unwrap().unwrap();
            assert!(matches!(ack, awc::ws::Frame::Text(_)));

            for generation in 1..=256 {
                server
                    .publish_federation_update(DevUpdate::new(
                        "beta".into(),
                        Some(format!("beta-{generation}").into()),
                        format!("beta-{}", generation + 1).into(),
                        generation + 1,
                        DevUpdateAction::TypesOnly,
                    ))
                    .unwrap();
            }
            assert!(matches!(
                beta_rx.try_recv(),
                Err(broadcast::error::TryRecvError::Lagged(_))
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(50), socket.next())
                    .await
                    .is_err(),
                "alpha received a sibling remote frame or full-reload control"
            );

            let alpha_update = DevUpdate::new(
                "alpha".into(),
                Some(manifest.build_id.clone()),
                "alpha-next".into(),
                2,
                DevUpdateAction::TypesOnly,
            );
            server
                .publish_federation_update(alpha_update.clone())
                .unwrap();
            let update = tokio::time::timeout(Duration::from_secs(1), socket.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let awc::ws::Frame::Text(update) = update else {
                panic!("expected alpha update text frame, got {update:?}");
            };
            assert_eq!(
                serde_json::from_slice::<DevUpdate>(&update).unwrap(),
                alpha_update
            );
        });
        server.close().unwrap();
    }

    #[test]
    fn running_remote_serves_manifest_and_build_scoped_head_assets() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-http-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let entry = root.path().join("src/container.js");
        std::fs::write(&entry, "export const containerReady = true;").unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry,
                quiet: true,
                federation: FederationBuildOptions {
                    enabled: true,
                    container_name: "catalog".to_owned(),
                    browser_target: "chromium>=120".to_owned(),
                    remote_entry_template: Some(format!(
                        "export const wakeDevBuildId={};",
                        serde_json::to_string(FEDERATION_BUILD_ID_PLACEHOLDER).unwrap()
                    )),
                    ..FederationBuildOptions::default()
                },
                ..ServeOptions::default()
            },
        )
        .unwrap();

        let response = http_get(port, "/wake-federation.json");
        let (headers, body) = response.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("HTTP/1.1 200"), "{headers}");
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("cache-control: no-store"),
            "{headers}"
        );
        let manifest: wake_federation_contract::Manifest = serde_json::from_str(body).unwrap();
        let development = manifest.development.as_ref().unwrap();
        assert_eq!(development.generation, 1);
        assert_eq!(
            development.updates_url,
            format!("ws://127.0.0.1:{port}/__wake_federation_updates?remote=catalog")
        );
        let remote_path = format!("/{}", manifest.remote_entry.url.trim_start_matches("./"));
        assert!(
            remote_path.contains(&format!("/builds/{}/", manifest.build_id)),
            "{remote_path}"
        );

        let head = http_request(port, "HEAD", &remote_path);
        let (head_headers, head_body) = head.split_once("\r\n\r\n").unwrap();
        let lower_headers = head_headers.to_ascii_lowercase();
        assert!(head_headers.starts_with("HTTP/1.1 200"), "{head_headers}");
        assert!(
            lower_headers.contains("content-type: text/javascript"),
            "{head_headers}"
        );
        assert!(
            lower_headers.contains("access-control-allow-origin: *"),
            "{head_headers}"
        );
        assert!(
            lower_headers.contains(&format!("content-length: {}", manifest.remote_entry.size)),
            "{head_headers}"
        );
        assert!(head_body.is_empty());

        let shadow_root = root
            .path()
            .join("public/@wake/federation/builds")
            .join(manifest.build_id.as_str());
        std::fs::create_dir_all(&shadow_root).unwrap();
        std::fs::write(
            shadow_root.join("not-exported.js"),
            "globalThis.__public_federation_shadow = true;",
        )
        .unwrap();
        std::fs::write(
            shadow_root.join("extensionless"),
            "public federation shadow",
        )
        .unwrap();
        let known_build_unknown_file = http_get(
            port,
            &format!(
                "/@wake/federation/builds/{}/not-exported.js",
                manifest.build_id
            ),
        );
        assert!(
            known_build_unknown_file.starts_with("HTTP/1.1 404"),
            "{known_build_unknown_file}"
        );
        assert!(!known_build_unknown_file.contains("__public_federation_shadow"));
        let reserved_extensionless = http_get(
            port,
            &format!(
                "/@wake/federation/builds/{}/extensionless",
                manifest.build_id
            ),
        );
        assert!(
            reserved_extensionless.starts_with("HTTP/1.1 404"),
            "{reserved_extensionless}"
        );
        assert!(!reserved_extensionless.contains("public federation shadow"));
        assert!(!reserved_extensionless.contains("<!doctype html>"));

        let mut broadcasts = server
            .inner
            .federation_senders
            .get("catalog")
            .unwrap()
            .subscribe();
        let gone_path = "/@wake/federation/builds/pruned-build/lazy.js";
        let cross_origin_head = http_request_with_headers(
            port,
            "HEAD",
            gone_path,
            "Origin: http://127.0.0.1:65530\r\n",
        );
        let (gone_headers, gone_body) = cross_origin_head.split_once("\r\n\r\n").unwrap();
        let gone_headers_lower = gone_headers.to_ascii_lowercase();
        assert!(gone_headers.starts_with("HTTP/1.1 410"), "{gone_headers}");
        assert!(gone_headers_lower.contains("access-control-allow-origin: *"));
        assert!(gone_headers_lower.contains("access-control-expose-headers:"));
        assert!(
            gone_headers_lower.contains(
                &format!("wake-federation-control: {FEDERATION_DEV_LEASE_SCHEMA_VERSION}")
                    .to_ascii_lowercase()
            )
        );
        assert!(gone_headers_lower.contains("wake-federation-action: full-reload"));
        assert!(gone_headers_lower.contains("wake-federation-remote: catalog"));
        assert!(gone_headers_lower.contains("wake-federation-expired-build-id: pruned-build"));
        assert!(gone_headers_lower.contains("wake-federation-generation: 1"));
        assert!(gone_body.is_empty());

        let gone_get = http_get(port, gone_path);
        let (gone_get_headers, gone_get_body) = gone_get.split_once("\r\n\r\n").unwrap();
        assert!(gone_get_headers.starts_with("HTTP/1.1 410"));
        let control: DevLeaseMessage = serde_json::from_str(gone_get_body).unwrap();
        assert_eq!(
            control,
            DevLeaseMessage::full_reload(
                "catalog".into(),
                manifest.build_id.clone(),
                1,
                Some("pruned-build".into()),
                DevLeaseReloadReason::BuildGone,
            )
        );
        assert!(matches!(
            broadcasts.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        server.close().unwrap();
    }

    #[test]
    fn running_federation_socket_acknowledges_current_and_reloads_only_invalid_lease() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-lease-ws-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let entry = root.path().join("src/container.js");
        std::fs::write(&entry, "export const containerReady = true;").unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry,
                quiet: true,
                federation: FederationBuildOptions {
                    enabled: true,
                    container_name: "catalog".to_owned(),
                    browser_target: "chromium>=120".to_owned(),
                    remote_entry_template: Some(format!(
                        "export const wakeDevBuildId={};",
                        serde_json::to_string(FEDERATION_BUILD_ID_PLACEHOLDER).unwrap()
                    )),
                    ..FederationBuildOptions::default()
                },
                ..ServeOptions::default()
            },
        )
        .unwrap();
        let manifest_response = http_get(port, "/wake-federation.json");
        let (_, manifest_body) = manifest_response.split_once("\r\n\r\n").unwrap();
        let manifest: wake_federation_contract::Manifest =
            serde_json::from_str(manifest_body).unwrap();

        actix_web::rt::System::new().block_on(async {
            let url = format!("ws://127.0.0.1:{port}/__wake_federation_updates?remote=catalog");
            let (_, mut socket) = awc::Client::new().ws(url).connect().await.unwrap();
            let lease = serde_json::to_string(&DevLeaseMessage::lease(
                "catalog".into(),
                vec![manifest.build_id.clone()],
            ))
            .unwrap();
            socket
                .send(awc::ws::Message::Text(lease.into()))
                .await
                .unwrap();
            let ack = socket.next().await.unwrap().unwrap();
            let awc::ws::Frame::Text(ack) = ack else {
                panic!("expected lease ack text frame, got {ack:?}");
            };
            assert_eq!(
                serde_json::from_slice::<DevLeaseMessage>(&ack).unwrap(),
                DevLeaseMessage::lease_ack(
                    "catalog".into(),
                    vec![manifest.build_id.clone()],
                    manifest.build_id.clone(),
                    1,
                )
            );

            let invalid = serde_json::to_string(&DevLeaseMessage::lease(
                "catalog".into(),
                vec!["not-retained".into()],
            ))
            .unwrap();
            socket
                .send(awc::ws::Message::Text(invalid.into()))
                .await
                .unwrap();
            let reload = socket.next().await.unwrap().unwrap();
            let awc::ws::Frame::Text(reload) = reload else {
                panic!("expected full reload text frame, got {reload:?}");
            };
            assert_eq!(
                serde_json::from_slice::<DevLeaseMessage>(&reload).unwrap(),
                DevLeaseMessage::full_reload(
                    "catalog".into(),
                    manifest.build_id.clone(),
                    1,
                    Some("not-retained".into()),
                    DevLeaseReloadReason::BuildGone,
                )
            );
        });
        server.close().unwrap();
    }

    #[test]
    fn default_html_has_hooks() {
        let h = default_html("/docs/", Some("rc-grid"), false);
        assert!(h.contains("/__wake/client.js"));
        assert!(h.contains("/docs/bundle.js"));
        assert!(h.contains("id=\"root\""));
        assert!(h.contains("window.__WAKE_MOUNT__=\"rc-grid\""));
        assert!(!h.contains("@wake/federation/bootstrap.mjs"));
        assert!(h.contains("<script src=\"/docs/bundle.js\"></script>"));
    }

    #[test]
    fn federation_bootstrap_is_the_single_ordered_application_entry() {
        let html = default_html("/docs/", Some("rc-grid"), true);
        assert!(
            html.contains(
                "<script type=\"module\" src=\"/docs/@wake/federation/bootstrap.mjs\"></script>"
            ),
            "{html}"
        );
        assert_eq!(html.matches("<script type=\"module\"").count(), 1, "{html}");
        assert!(!html.contains("src=\"/docs/bundle.js\""), "{html}");
        assert!(!html.contains("standalone.mjs"), "{html}");
    }

    #[test]
    fn federation_bootstrap_route_uses_javascript_mime() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-bootstrap-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let entry = root.path().join("src/index.js");
        std::fs::write(&entry, "globalThis.__wake_app_loaded = true;").unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry,
                quiet: true,
                federation: FederationBuildOptions {
                    bootstrap: Some("globalThis.__wake_broker_ready = true;".to_string()),
                    ..test_federation_options("app")
                },
                ..ServeOptions::default()
            },
        )
        .unwrap();

        let html = http_get(port, "/");
        let bootstrap = http_get(port, "/@wake/federation/bootstrap.mjs");
        assert!(html.contains("/@wake/federation/bootstrap.mjs"), "{html}");
        assert!(!html.contains("src=\"/bundle.js\""), "{html}");
        assert!(
            bootstrap
                .to_ascii_lowercase()
                .contains("content-type: text/javascript"),
            "{bootstrap}"
        );
        assert!(bootstrap.contains("globalThis.__wake_broker_ready = true;"));
        assert!(
            bootstrap.contains("await import(new URL('../../bundle.js',import.meta.url).href);"),
            "{bootstrap}"
        );
        server.close().unwrap();
    }

    #[test]
    fn federation_publish_hook_emits_the_structured_server_event() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-federation-publish-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        let entry = root.path().join("src/index.js");
        std::fs::write(&entry, "export const ready = true;").unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let events = Arc::new(Mutex::new(Vec::<ServerEvent>::new()));
        let captured = Arc::clone(&events);
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry,
                quiet: true,
                event_handler: Some(Arc::new(move |event| {
                    captured.lock().unwrap().push(event);
                })),
                federation: test_federation_options("catalog"),
                ..ServeOptions::default()
            },
        )
        .unwrap();
        let mut update = DevUpdate::new(
            "catalog".into(),
            Some("old".into()),
            "new".into(),
            3,
            DevUpdateAction::TypesOnly,
        );
        update.changed_exposes = vec!["./Button".into()];
        update.types_hash = Some("types-3".to_string());

        server.publish_federation_update(update).unwrap();

        assert!(events.lock().unwrap().iter().any(|event| matches!(
            event,
            ServerEvent::FederationUpdated {
                remote,
                old_build_id: Some(old_build_id),
                new_build_id,
                changed_exposes,
                types_hash: Some(types_hash),
                action: DevUpdateAction::TypesOnly,
            } if remote == "catalog"
                && old_build_id == "old"
                && new_build_id == "new"
                && changed_exposes == &["./Button"]
                && types_hash == "types-3"
        )));
        server.close().unwrap();
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
        let _network_guard = lock_network_test();
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
                    file_system: None,
                    resolve_options: ResolveOptions::default(),
                    define: Vec::new(),
                    target_env: TargetEnv::default(),
                    jsx_import_source: "react".to_string(),
                    watch_interests: vec![WatchInterest::tree(workspace.path().join("src"))],
                    refresh: None,
                    federation: FederationBuildOptions::default(),
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
        let _network_guard = lock_network_test();
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
                file_system: None,
                resolve_options: ResolveOptions::default(),
                define: Vec::new(),
                target_env: TargetEnv::default(),
                jsx_import_source: "react".to_string(),
                watch_interests: vec![WatchInterest::tree(root.path().join("src"))],
                refresh: None,
                federation: FederationBuildOptions::default(),
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
                .filter(|event| matches!(
                    event,
                    ServerEvent::Rebuilt {
                        workspace: Some(_),
                        ..
                    }
                ))
                .count(),
            0,
            "the startup coverage Rescan must not eagerly build lazy mounts"
        );
        assert_eq!(
            captured
                .iter()
                .filter(|event| matches!(
                    event,
                    ServerEvent::Rebuilt {
                        workspace: None,
                        ..
                    }
                ))
                .count(),
            2,
            "the primary app builds its initial and post-registration Rescan generations"
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
    #[allow(clippy::result_large_err)]
    fn lazy_candidate_is_registered_but_not_materialized_until_first_request() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-lazy-candidate-site-")
            .tempdir()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("wake-dev-lazy-candidate-workspace-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/index.js"),
            "export const site = true;",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("src/index.js"),
            "export const workspace = true;",
        )
        .unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);
        let materializations = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(Mutex::new(Vec::new()));
        let refresh_materializations = Arc::clone(&materializations);
        let refresh_completed = Arc::clone(&completed);
        let candidate_interest = workspace.path().join("candidate-source");
        let refresh: RefreshMount = Arc::new(move |current, _| {
            let plan = current.clone();
            let materializations = Arc::clone(&refresh_materializations);
            let completed = Arc::clone(&refresh_completed);
            let preliminary = WatchInterest::tree(candidate_interest.clone());
            Ok(DevMountRefresh::Candidate(DevMountCandidate::new(
                vec![preliminary.clone()],
                move || {
                    materializations.fetch_add(1, Ordering::SeqCst);
                    Ok(DevMountMaterialization {
                        plan,
                        watch_interests: vec![preliminary],
                        generated_paths: Vec::new(),
                    })
                },
                move |outcome| completed.lock().unwrap().push(outcome),
            )))
        });
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                mounts: vec![MountedServeOptions {
                    name: "lazy".to_owned(),
                    root: workspace.path().to_path_buf(),
                    base_path: "/lazy/".to_owned(),
                    loading: DevLoading::Lazy,
                    entry: workspace.path().join("src/index.js"),
                    resolve_options: ResolveOptions::default(),
                    define: Vec::new(),
                    target_env: TargetEnv::default(),
                    jsx_import_source: "react".to_owned(),
                    file_system: None,
                    watch_interests: Vec::new(),
                    refresh: Some(refresh),
                    federation: FederationBuildOptions::default(),
                }],
                ..ServeOptions::default()
            },
        )
        .unwrap();

        assert_eq!(materializations.load(Ordering::SeqCst), 0);
        assert!(completed.lock().unwrap().is_empty());
        let response = http_get(port, "/lazy/bundle.js");
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(materializations.load(Ordering::SeqCst), 1);
        assert_eq!(*completed.lock().unwrap(), vec![RefreshOutcome::Committed]);
        server.close().unwrap();
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn lazy_load_rejection_registers_new_recovery_interest_before_returning() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-lazy-rejected-site-")
            .tempdir()
            .unwrap();
        let workspace = tempfile::Builder::new()
            .prefix("wake-dev-lazy-rejected-workspace-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::create_dir_all(workspace.path().join("src")).unwrap();
        std::fs::write(
            root.path().join("src/index.js"),
            "export const site = true;",
        )
        .unwrap();
        std::fs::write(
            workspace.path().join("src/index.js"),
            "export const workspace = true;",
        )
        .unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let calls = Arc::new(AtomicUsize::new(0));
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let recovery_file = workspace.path().join("recovery/control.token");
        let refresh_calls = Arc::clone(&calls);
        let refresh_outcomes = Arc::clone(&outcomes);
        let refresh_recovery_file = recovery_file.clone();
        let plan = DevMountPlan {
            entry: workspace.path().join("src/index.js"),
            resolve_options: ResolveOptions::default(),
            define: Vec::new(),
            target_env: TargetEnv::default(),
            jsx_import_source: "react".to_owned(),
            file_system: Arc::new(OsFileSystem),
        };
        let refresh: DeferredRefreshMount =
            Arc::new(
                move |_| match refresh_calls.fetch_add(1, Ordering::SeqCst) {
                    0 => Ok(DevMountRefresh::Invalidate {
                        generated_paths: Vec::new(),
                    }),
                    1 => Ok(DevMountRefresh::RejectedCandidate {
                        watch_interests: vec![WatchInterest::exact_file(&refresh_recovery_file)],
                        diagnostic: Diagnostic::error("missing recovery control")
                            .with_code("WAKE_TEST_RECOVERY"),
                    }),
                    _ => {
                        let plan = plan.clone();
                        let recovery_interest =
                            WatchInterest::exact_file(refresh_recovery_file.clone());
                        let completed = Arc::clone(&refresh_outcomes);
                        Ok(DevMountRefresh::Candidate(DevMountCandidate::new(
                            vec![recovery_interest.clone()],
                            move || {
                                Ok(DevMountMaterialization {
                                    plan,
                                    watch_interests: vec![recovery_interest],
                                    generated_paths: Vec::new(),
                                })
                            },
                            move |outcome| completed.lock().unwrap().push(outcome),
                        )))
                    }
                },
            );
        let server = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                deferred_mounts: vec![DeferredMountedServeOptions {
                    name: "lazy".to_owned(),
                    root: workspace.path().to_path_buf(),
                    base_path: "/lazy/".to_owned(),
                    watch_interests: Vec::new(),
                    refresh,
                    federation: FederationBuildOptions::default(),
                }],
                ..ServeOptions::default()
            },
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let response = http_get(port, "/lazy/bundle.js");
        assert!(!response.starts_with("HTTP/1.1 200"), "{response}");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        std::fs::create_dir_all(recovery_file.parent().unwrap()).unwrap();
        std::fs::write(&recovery_file, "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while calls.load(Ordering::SeqCst) < 3 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            calls.load(Ordering::SeqCst) >= 3,
            "the rejected candidate's new exact interest was not registered"
        );
        let mut response = String::new();
        while Instant::now() < deadline {
            response = http_get(port, "/lazy/bundle.js");
            if response.starts_with("HTTP/1.1 200") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        assert!(
            outcomes
                .lock()
                .unwrap()
                .contains(&RefreshOutcome::Committed)
        );
        server.close().unwrap();
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn startup_rescan_failure_is_not_reported_as_ready() {
        let _network_guard = lock_network_test();
        let root = tempfile::Builder::new()
            .prefix("wake-dev-startup-rescan-failure-")
            .tempdir()
            .unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/index.js"), "export default 1;").unwrap();
        let reservation = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = reservation.local_addr().unwrap().port();
        drop(reservation);

        let result = start(
            root.path(),
            port,
            ServeOptions {
                entry: root.path().join("src/index.js"),
                quiet: true,
                refresh: Some(Arc::new(|_, invalidation| {
                    assert!(invalidation.is_rescan());
                    Ok(DevMountRefresh::RestartRequired {
                        reason: "startup configuration changed".to_owned(),
                    })
                })),
                ..ServeOptions::default()
            },
        );
        let error = match result {
            Ok(server) => {
                server.close().unwrap();
                panic!("startup must remain behind the failed Rescan fence")
            }
            Err(error) => error,
        };
        assert!(error.to_string().contains("startup configuration changed"));
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
    federation_senders: BTreeMap<String, broadcast::Sender<String>>,
    event_handler: Option<EventHandler>,
}

struct ServerInner {
    url: String,
    handle: actix_web::dev::ServerHandle,
    stop: Arc<StopSignal>,
    join: Mutex<Option<JoinHandle<std::io::Result<()>>>>,
    closed: AtomicBool,
    federation_senders: BTreeMap<String, broadcast::Sender<String>>,
    event_handler: Option<EventHandler>,
}

#[derive(Clone)]
pub struct ServerHandle {
    inner: Arc<ServerInner>,
}

impl ServerHandle {
    pub fn url(&self) -> &str {
        &self.inner.url
    }

    /// Publish a validated, versioned federation update to connected WebSocket clients.
    ///
    /// Classification is intentionally supplied by the artifact coordinator. The dev server does
    /// not infer whether a declaration-only sync, isolated remount, or page reload is safe from a
    /// filesystem event alone.
    pub fn publish_federation_update(&self, update: DevUpdate) -> std::io::Result<()> {
        let update = update.normalized();
        update
            .validate()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        let message = msg_federation_update(&update)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let sender = self
            .inner
            .federation_senders
            .get(update.remote.as_str())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "Federation remote `{}` is not mounted by this Wake dev server",
                        update.remote
                    ),
                )
            })?;
        emit_federation_server_event(&update, self.inner.event_handler.as_ref());
        // A publisher remains successful when no browser is connected yet; future subscribers
        // receive subsequent generations and the caller still gets its structured server event.
        let _ = sender.send(message);
        Ok(())
    }

    /// Request shutdown without joining worker threads. Safe for language-runtime finalizers.
    pub fn request_close(&self) {
        self.inner.stop.request();
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
            self.inner.stop.request();
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
        self.stop.request();
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
    let stop = Arc::new(StopSignal::new());
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
            federation_senders: started.federation_senders,
            event_handler: started.event_handler,
        }),
    })
}
