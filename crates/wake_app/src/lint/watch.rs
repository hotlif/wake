//! RAII filesystem monitoring and bounded latest-result scheduling for versioned lint contexts.
use super::{LintContext, LintSnapshot};
use crate::{CancellationToken, WakeError};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const DEBOUNCE: Duration = Duration::from_millis(50);

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum LintWatchEvent {
    CheckStart { generation: u64 },
    Checked { snapshot: Box<LintSnapshot> },
    Diagnostic { generation: u64, error: WakeError },
}
impl LintWatchEvent {
    pub fn generation(&self) -> u64 {
        match self {
            Self::CheckStart { generation } | Self::Diagnostic { generation, .. } => *generation,
            Self::Checked { snapshot } => snapshot.generation,
        }
    }
}

struct State {
    closed: bool,
    dirty: bool,
    last_change: Instant,
    backend_epoch: u64,
    reconcile: bool,
    backend_error: Option<WakeError>,
    active: Option<CancellationToken>,
    started: Option<LintWatchEvent>,
    completed: Option<LintWatchEvent>,
}
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}
impl Shared {
    fn changed(&self) {
        let mut state = lock(&self.state);
        if !state.closed {
            state.dirty = true;
            state.last_change = Instant::now();
            self.changed.notify_all();
        }
    }
    fn stop(&self) {
        let mut state = lock(&self.state);
        state.closed = true;
        state.started = None;
        state.completed = None;
        if let Some(token) = state.active.take() {
            token.cancel();
        }
        self.changed.notify_all();
    }
}

pub struct LintWatcher {
    context: LintContext,
    shared: Arc<Shared>,
    _observer: Arc<dyn Fn() + Send + Sync>,
    join: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
fn watch_error(error: impl std::fmt::Display) -> WakeError {
    WakeError::new("WAKE_LINT_WATCH", error.to_string())
}

fn parent_watch_path(root: &Path) -> Option<&Path> {
    root.parent()?.ancestors().find(|path| path.is_dir())
}

fn path_key(path: &Path) -> PathBuf {
    let path = wake_common::fs::normalize(path);
    #[cfg(windows)]
    let path = PathBuf::from(path.as_os_str().to_ascii_lowercase());
    path
}

#[derive(Default)]
struct Inputs {
    baseline: Option<crate::WatchInterest>,
    dependencies: BTreeSet<PathBuf>,
}
impl Inputs {
    fn matches(&self, path: &Path, structural: bool) -> bool {
        if self
            .baseline
            .as_ref()
            .is_some_and(|baseline| baseline.matches_event(path, structural))
        {
            return true;
        }
        let path = path_key(path);
        self.dependencies.contains(&path)
            || structural
                && self
                    .dependencies
                    .range(path.clone()..)
                    .next()
                    .is_some_and(|dependency| dependency.starts_with(path))
    }
}

struct Coverage {
    dependencies: Arc<BTreeSet<PathBuf>>,
    registrations: BTreeMap<PathBuf, RecursiveMode>,
    identities: Vec<(PathBuf, Option<same_file::Handle>)>,
}
impl Coverage {
    fn new(context: &LintContext) -> Self {
        let root = wake_common::fs::normalize(&context.options().root);
        let dependencies = context.watch_dependencies();
        let root_key = path_key(&root);
        let mut registrations = BTreeMap::new();
        if let Some(parent) = parent_watch_path(&root) {
            registrations.insert(parent.to_owned(), RecursiveMode::NonRecursive);
        }
        if root.is_dir() {
            registrations.insert(root.clone(), RecursiveMode::Recursive);
        }
        // Coalesce before probing the filesystem: hundreds of files usually share one parent.
        let parents: BTreeSet<_> = dependencies
            .iter()
            .filter_map(|path| path.parent())
            .collect();
        for parent in parents {
            let parent_key = path_key(parent);
            // The root registration already covers dependencies inside the project. Dependency
            // observations also retain every ancestor directory for snapshot identity; those
            // ancestors must not make us register broad, unrelated filesystem roots.
            if parent_key.starts_with(&root_key) || root_key.starts_with(&parent_key) {
                continue;
            }
            if let Some(existing) = parent.ancestors().find(|path| path.is_dir()) {
                let existing_key = path_key(existing);
                if existing_key.starts_with(&root_key) || root_key.starts_with(&existing_key) {
                    continue;
                }
                registrations
                    .entry(existing.to_owned())
                    .or_insert(RecursiveMode::NonRecursive);
            }
        }
        // Include the missing root itself so creation still recovers if a backend drops an event.
        let mut probes: BTreeSet<_> = registrations.keys().cloned().collect();
        probes.insert(root);
        let identities = probes
            .into_iter()
            .map(|path| {
                let identity = same_file::Handle::from_path(&path).ok();
                (path, identity)
            })
            .collect();
        Self {
            dependencies,
            registrations,
            identities,
        }
    }
    fn changed(&self, context: &LintContext) -> bool {
        *self.dependencies != *context.watch_dependencies()
            || self
                .identities
                .iter()
                .any(|(path, identity)| *identity != same_file::Handle::from_path(path).ok())
    }
}

impl LintWatcher {
    pub fn start(context: LintContext) -> Result<Self, WakeError> {
        if context.is_closed() {
            return Err(WakeError::closed("LintContext"));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                closed: false,
                dirty: true,
                last_change: Instant::now(),
                backend_epoch: 0,
                reconcile: false,
                backend_error: None,
                active: None,
                started: None,
                completed: None,
            }),
            changed: Condvar::new(),
        });
        let signal = shared.clone();
        let observer: Arc<dyn Fn() + Send + Sync> = Arc::new(move || signal.changed());
        context.subscribe(&observer);
        let coverage = Coverage::new(&context);
        let backend = install_backend(&context, &shared, &coverage)?;
        let worker_context = context.clone();
        let worker_shared = shared.clone();
        let join = thread::Builder::new()
            .name("wake-lint-watch".into())
            .spawn(move || {
                run(worker_context, worker_shared.clone(), backend, coverage);
                worker_shared.stop();
            })
            .map_err(watch_error)?;
        Ok(Self {
            context,
            shared,
            _observer: observer,
            join: Mutex::new(Some(join)),
        })
    }

    pub fn drain_events(&self) -> Vec<LintWatchEvent> {
        let mut state = lock(&self.shared.state);
        if state.closed || self.context.is_closed() {
            return Vec::new();
        }
        let generation = self.context.generation();
        let mut events = Vec::new();
        if let Some(event) = state.started.take()
            && event.generation() == generation
        {
            events.push(event);
        }
        if let Some(event) = state.completed.take()
            && event.generation() == generation
        {
            events.push(event);
        }
        events
    }

    pub fn is_watching(&self) -> bool {
        !lock(&self.shared.state).closed && !self.context.is_closed()
    }
    pub fn request_stop(&self) {
        self.shared.stop();
    }
    pub fn poll_stopped(&self) -> bool {
        let mut join = lock(&self.join);
        if join.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        if let Some(join) = join.take() {
            let _ = join.join();
        }
        true
    }
    pub fn stop(&self) {
        self.request_stop();
        if let Some(join) = lock(&self.join).take() {
            let _ = join.join();
        }
    }
}
impl Drop for LintWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn install_backend(
    context: &LintContext,
    shared: &Arc<Shared>,
    coverage: &Coverage,
) -> Result<RecommendedWatcher, WakeError> {
    let epoch = {
        let mut state = lock(&shared.state);
        state.backend_epoch += 1;
        state.backend_epoch
    };
    let root = wake_common::fs::normalize(&context.options().root);
    match root.canonicalize() {
        Ok(actual) => super::context::validate_root_identity(&root, &actual)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(super::io_error(&root, error)),
    }
    let mut tree = crate::WatchInterest::all_files_tree(&root);
    for directory in [
        ".wake",
        ".git",
        ".yarn",
        "node_modules",
        "dist",
        "docs-dist",
    ] {
        tree = tree.excluding_tree(root.join(directory));
    }
    let tree = tree.resolve_against(&root);
    let baseline = context.options().baseline.as_ref().map(|baseline| {
        crate::WatchInterest::exact_file(root.join(&baseline.path)).resolve_against(&root)
    });
    let inputs = Inputs {
        baseline,
        dependencies: coverage
            .dependencies
            .iter()
            .map(|path| path_key(path))
            .collect(),
    };
    let callback_context = context.clone();
    let callback_shared = shared.clone();
    let callback_root = root.clone();
    let mut watcher = notify::recommended_watcher(move |event| {
        receive_event(
            &callback_context,
            &callback_shared,
            epoch,
            &callback_root,
            &tree,
            &inputs,
            event,
        );
    })
    .map_err(watch_error)?;
    // Nonrecursive ancestor coverage detects removal/replacement of the watched root itself.
    // Creating a missing root is followed by a new recursive registration and authoritative scan.
    for (path, mode) in &coverage.registrations {
        watcher.watch(path, *mode).map_err(watch_error)?;
    }
    Ok(watcher)
}

fn receive_event(
    context: &LintContext,
    shared: &Shared,
    epoch: u64,
    root: &Path,
    tree: &crate::WatchInterest,
    inputs: &Inputs,
    event: notify::Result<Event>,
) {
    let mut state = lock(&shared.state);
    if state.closed || state.backend_epoch != epoch {
        return;
    }
    let (reconcile, error) = match event {
        Err(error) => (true, Some(watch_error(error))),
        Ok(event) => {
            let structural = matches!(
                event.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(notify::event::ModifyKind::Name(_))
            );
            let rescan = event.need_rescan();
            // Parent registrations may report a directory timestamp change when an ignored
            // output child is created. Child events carry content changes; structural events
            // and periodic identity probes cover directory removal/replacement.
            if !rescan
                && !structural
                && !event.paths.is_empty()
                && event.paths.iter().all(|path| path.is_dir())
            {
                return;
            }
            if !rescan
                && !matches!(
                    event.kind,
                    EventKind::Any
                        | EventKind::Create(_)
                        | EventKind::Modify(_)
                        | EventKind::Remove(_)
                )
            {
                return;
            }
            if !rescan
                && !event.paths.iter().any(|path| {
                    tree.matches_event(path, structural) || inputs.matches(path, structural)
                })
            {
                return;
            }
            let root_replaced = structural
                && event.paths.iter().any(|path| {
                    let path = wake_common::fs::normalize(path);
                    #[cfg(windows)]
                    {
                        root.to_string_lossy().to_ascii_lowercase().starts_with(
                            &(path
                                .to_string_lossy()
                                .to_ascii_lowercase()
                                .trim_end_matches('\\')
                                .to_owned()
                                + "\\"),
                        ) || root
                            .to_string_lossy()
                            .eq_ignore_ascii_case(&path.to_string_lossy())
                    }
                    #[cfg(not(windows))]
                    {
                        root.starts_with(path)
                    }
                });
            (
                root_replaced
                    || rescan
                    || structural && event.paths.iter().any(|path| inputs.matches(path, true)),
                None,
            )
        }
    };
    // This gate also retires old callbacks. Silent invalidation avoids recursively notifying our
    // own weak context subscription while holding the backend publication gate.
    if context.invalidate_silent().is_err() {
        return;
    }
    state.dirty = true;
    state.reconcile |= reconcile;
    state.backend_error = error;
    state.last_change = Instant::now();
    shared.changed.notify_all();
}

fn run(
    context: LintContext,
    shared: Arc<Shared>,
    backend: RecommendedWatcher,
    mut coverage: Coverage,
) {
    let mut backend = Some(backend);
    let mut retry = Duration::from_millis(250);
    loop {
        let mut state = lock(&shared.state);
        loop {
            if state.closed || context.is_closed() {
                return;
            }
            if state.dirty {
                let remaining = DEBOUNCE.saturating_sub(state.last_change.elapsed());
                if remaining.is_zero() {
                    break;
                }
                state = shared
                    .changed
                    .wait_timeout(state, remaining)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            } else {
                let (next, waited) = shared
                    .changed
                    .wait_timeout(state, Duration::from_millis(250))
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                state = next;
                if waited.timed_out() {
                    drop(state);
                    let changed = coverage.changed(&context);
                    state = lock(&shared.state);
                    if changed {
                        if context.invalidate_silent().is_err() {
                            return;
                        }
                        state.backend_epoch += 1;
                        state.reconcile = true;
                        state.dirty = true;
                        state.last_change = Instant::now();
                    }
                }
            }
        }
        state.dirty = false;
        let reconcile = std::mem::take(&mut state.reconcile);
        let error = state.backend_error.take();
        if let Some(error) = error {
            state.completed = Some(LintWatchEvent::Diagnostic {
                generation: context.generation(),
                error,
            });
        }
        drop(state);
        if reconcile || backend.is_none() {
            // Retire before dropping the backend so queued callbacks cannot revoke its successor.
            {
                lock(&shared.state).backend_epoch += 1;
            }
            drop(backend.take());
            let current = Coverage::new(&context);
            match install_backend(&context, &shared, &current) {
                Ok(watcher) => {
                    backend = Some(watcher);
                    coverage = current;
                    retry = Duration::from_millis(250);
                }
                Err(error) => {
                    let mut state = lock(&shared.state);
                    state.completed = Some(LintWatchEvent::Diagnostic {
                        generation: context.generation(),
                        error,
                    });
                    state.dirty = true;
                    state.reconcile = true;
                    state = shared
                        .changed
                        .wait_timeout(state, retry)
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .0;
                    state.last_change = Instant::now();
                    retry = (retry * 2).min(Duration::from_secs(5));
                    continue;
                }
            }
        }
        let cancellation = CancellationToken::default();
        let request = {
            let mut state = lock(&shared.state);
            if state.closed || context.is_closed() {
                return;
            }
            let Ok(request) = context.prepare_check(cancellation.clone()) else {
                return;
            };
            state.active = Some(cancellation);
            state.started = Some(LintWatchEvent::CheckStart {
                generation: request.generation(),
            });
            request
        };
        let generation = request.generation();
        let result = catch_unwind(AssertUnwindSafe(|| request.run())).unwrap_or_else(|_| {
            Err(WakeError::new(
                "WAKE_INTERNAL",
                "Lint watch analysis panicked",
            ))
        });
        let mut state = lock(&shared.state);
        state.active = None;
        if state.closed || context.is_closed() {
            return;
        }
        if context.generation() != generation {
            continue;
        }
        if *coverage.dependencies != *context.watch_dependencies() {
            // No result read before registration may become authoritative. Reinstall the new
            // filter and coverage, then scan again to close the read/registration event gap.
            if context.invalidate_silent().is_err() {
                return;
            }
            state.backend_epoch += 1;
            state.reconcile = true;
            state.dirty = true;
            state.last_change = Instant::now();
            continue;
        }
        state.completed = match result {
            Ok(snapshot) => Some(LintWatchEvent::Checked {
                snapshot: Box::new(snapshot),
            }),
            Err(error) if error.code != "WAKE_CANCELLED" => {
                Some(LintWatchEvent::Diagnostic { generation, error })
            }
            _ => None,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_interests_match_exact_paths_and_structural_ancestors() {
        let root = wake_common::fs::normalize(&std::env::temp_dir()).join("watch-inputs");
        let inputs = Inputs {
            baseline: None,
            dependencies: [
                root.join("cache/pkg.zip"),
                root.join("missing/new/index.ts"),
            ]
            .iter()
            .map(|path| path_key(path))
            .collect(),
        };
        assert!(inputs.matches(&root.join("cache/pkg.zip"), false));
        assert!(inputs.matches(&root.join("missing"), true));
        assert!(inputs.matches(&root.join("missing/new"), true));
        assert!(!inputs.matches(&root.join("missing"), false));
        assert!(!inputs.matches(&root.join("miss"), true));
        assert!(!inputs.matches(&root.join("missing/unrelated.ts"), true));
        assert!(!inputs.matches(&root.join("cache/pkg.zip.backup"), true));
        #[cfg(windows)]
        assert!(inputs.matches(&root.join("CACHE/PKG.ZIP"), false));
    }

    fn next_checked(watcher: &LintWatcher) -> u64 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            for event in watcher.drain_events() {
                if let LintWatchEvent::Checked { snapshot } = event {
                    return snapshot.generation;
                }
            }
            assert!(Instant::now() < deadline, "watch recovery did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn backend_failure_recovers_and_retired_callbacks_cannot_cancel_the_successor() {
        let root = tempfile::tempdir().unwrap();
        let context = LintContext::create(super::super::LintProjectOptions {
            root: root.path().into(),
            ..Default::default()
        })
        .unwrap();
        let watcher = LintWatcher::start(context.clone()).unwrap();
        let first = next_checked(&watcher);
        let epoch = lock(&watcher.shared.state).backend_epoch;
        let tree = crate::WatchInterest::all_files_tree(root.path());
        receive_event(
            &context,
            &watcher.shared,
            epoch,
            root.path(),
            &tree,
            &Inputs::default(),
            Err(notify::Error::generic("injected backend loss")),
        );
        let recovered = next_checked(&watcher);
        assert!(recovered > first);
        assert!(lock(&watcher.shared.state).backend_epoch > epoch);
        let request = context.prepare_check(CancellationToken::default()).unwrap();
        let generation = request.generation();
        receive_event(
            &context,
            &watcher.shared,
            epoch,
            root.path(),
            &tree,
            &Inputs::default(),
            Err(notify::Error::generic("retired callback")),
        );
        assert_eq!(context.generation(), generation);
        assert!(request.run().is_ok());
        watcher.stop();
        context.close();
    }

    #[test]
    fn cache_publication_and_ignored_output_changes_do_not_create_a_watch_loop() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a.js"), "debugger;").unwrap();
        let context = LintContext::create(super::super::LintProjectOptions {
            root: root.path().into(),
            cache: true,
            ..Default::default()
        })
        .unwrap();
        let watcher = LintWatcher::start(context.clone()).unwrap();
        next_checked(&watcher);
        thread::sleep(Duration::from_millis(200));
        watcher.drain_events();
        let generation = context.generation();
        std::fs::write(root.path().join(".wake/irrelevant.json"), "{}").unwrap();
        thread::sleep(Duration::from_millis(300));
        assert_eq!(context.generation(), generation);
        assert!(watcher.drain_events().is_empty());
        watcher.stop();
        context.close();
    }
}
