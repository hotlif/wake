//! Versioned project input snapshots and single-analysis lifecycle. No frontend event loop.
use super::{
    LintBaselineMode, LintFixMode, LintProjectOptions, LintProjectResult, failure,
    normalize_relative,
};
use crate::{CancellationToken, WakeError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

pub(super) fn validate_root_identity(expected: &Path, actual: &Path) -> Result<(), WakeError> {
    let expected_normalized = wake_common::fs::normalize(expected);
    let actual_normalized = wake_common::fs::normalize(actual);
    #[cfg(windows)]
    let equal = expected_normalized
        .to_string_lossy()
        .eq_ignore_ascii_case(&actual_normalized.to_string_lossy());
    #[cfg(not(windows))]
    let equal = expected_normalized == actual_normalized;
    if equal {
        Ok(())
    } else {
        Err(WakeError::new("WAKE_LINT_IO", "Lint context root was redirected to another project; create a new context for that root").at(expected))
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintDocument {
    pub filename: String,
    pub version: i64,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct LintSnapshot {
    pub generation: u64,
    pub documents: BTreeMap<String, i64>,
    pub result: LintProjectResult,
}

#[derive(Default)]
struct State {
    closed: bool,
    generation: u64,
    versions: BTreeMap<String, i64>,
    documents: BTreeMap<String, Arc<LintDocument>>,
    dependencies: Arc<BTreeSet<PathBuf>>,
    active: Option<CancellationToken>,
}

impl State {
    fn ensure_open(&self) -> Result<(), WakeError> {
        if self.closed {
            Err(WakeError::closed("LintContext"))
        } else {
            Ok(())
        }
    }
    fn advance(&mut self) -> Result<(), WakeError> {
        self.ensure_open()?;
        if self.generation >= MAX_SAFE_INTEGER as u64 {
            return Err(failure("Lint context generation exhausted"));
        }
        self.generation += 1;
        if let Some(active) = self.active.take() {
            active.cancel();
        }
        Ok(())
    }
}

struct Inner {
    options: LintProjectOptions,
    state: Mutex<State>,
    executing: Mutex<bool>,
    ready: Condvar,
    observers: Mutex<Vec<Weak<dyn Fn() + Send + Sync>>>,
}

#[derive(Clone)]
pub struct LintContext {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct Execution<'a>(&'a Inner);
impl Drop for Execution<'_> {
    fn drop(&mut self) {
        *lock(&self.0.executing) = false;
        self.0.ready.notify_all();
    }
}

impl LintContext {
    pub fn create(options: LintProjectOptions) -> Result<Self, WakeError> {
        Self::create_inner(options, true)
    }

    /// Register monitoring before reporting disk configuration errors, allowing live recovery.
    pub fn create_for_watch(options: LintProjectOptions) -> Result<Self, WakeError> {
        Self::create_inner(options, false)
    }

    fn create_inner(
        mut options: LintProjectOptions,
        validate_configuration: bool,
    ) -> Result<Self, WakeError> {
        if options.stdin.is_some()
            || options.fix != LintFixMode::Off
            || options.print_config.is_some()
            || options.list_rules
            || options
                .baseline
                .as_ref()
                .is_some_and(|baseline| baseline.mode != LintBaselineMode::Check)
        {
            return Err(failure(
                "LintContext only supports read-only project checks and existing baselines",
            ));
        }
        options.root = options
            .root
            .canonicalize()
            .map_err(|e| super::io_error(&options.root, e))?;
        if !options.root.is_dir() {
            return Err(failure("lint root must be a directory"));
        }
        for path in &options.paths {
            let relative = normalize_relative(path)?;
            if relative.contains(['*', '?']) {
                super::glob(&relative)?;
            }
        }
        if let Some(baseline) = &mut options.baseline {
            baseline.path = normalize_relative(&baseline.path)?;
            if !baseline.path.ends_with(".json") {
                return Err(failure("baseline requires a .json filename"));
            }
        }
        let lint = if validate_configuration {
            wake_config::load(&options.root)
                .map_err(|e| failure(e.to_string()))?
                .lint
        } else {
            wake_config::Lint::default()
        };
        super::ConfiguredLint::new(
            lint,
            options.rules.clone(),
            options.globals.clone(),
            options.environments.clone(),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                state: Mutex::new(State::default()),
                executing: Mutex::new(false),
                ready: Condvar::new(),
                observers: Mutex::new(Vec::new()),
            }),
        })
    }

    fn validate_version(state: &State, path: &str, version: i64) -> Result<(), WakeError> {
        state.ensure_open()?;
        if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&version)
            || state
                .versions
                .get(path)
                .is_some_and(|previous| version <= *previous)
        {
            return Err(failure(
                "Lint document version must be a strictly increasing safe integer",
            ));
        }
        Ok(())
    }

    pub fn update_document(&self, mut document: LintDocument) -> Result<(), WakeError> {
        document.filename = normalize_relative(&document.filename)?;
        super::source_type(&document.filename)?;
        let mut state = lock(&self.inner.state);
        Self::validate_version(&state, &document.filename, document.version)?;
        state.advance()?;
        state
            .versions
            .insert(document.filename.clone(), document.version);
        state
            .documents
            .insert(document.filename.clone(), Arc::new(document));
        self.inner.ready.notify_all();
        drop(state);
        self.notify_changed();
        Ok(())
    }

    pub fn close_document(&self, filename: &str, version: i64) -> Result<(), WakeError> {
        let path = normalize_relative(filename)?;
        let mut state = lock(&self.inner.state);
        Self::validate_version(&state, &path, version)?;
        if !state.documents.contains_key(&path) {
            return Err(failure("Lint document is not open"));
        }
        state.advance()?;
        state.versions.insert(path.clone(), version);
        state.documents.remove(&path);
        self.inner.ready.notify_all();
        drop(state);
        self.notify_changed();
        Ok(())
    }

    pub fn invalidate(&self) -> Result<u64, WakeError> {
        let generation = self.invalidate_silent()?;
        self.notify_changed();
        Ok(generation)
    }

    pub(super) fn invalidate_silent(&self) -> Result<u64, WakeError> {
        let mut state = lock(&self.inner.state);
        state.advance()?;
        self.inner.ready.notify_all();
        Ok(state.generation)
    }

    pub(super) fn options(&self) -> &LintProjectOptions {
        &self.inner.options
    }

    pub(super) fn watch_dependencies(&self) -> Arc<BTreeSet<PathBuf>> {
        lock(&self.inner.state).dependencies.clone()
    }

    pub(super) fn subscribe(&self, observer: &Arc<dyn Fn() + Send + Sync>) {
        let mut observers = lock(&self.inner.observers);
        observers.retain(|observer| observer.strong_count() > 0);
        observers.push(Arc::downgrade(observer));
    }

    fn notify_changed(&self) {
        let observers: Vec<_> = lock(&self.inner.observers)
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for observer in observers {
            observer();
        }
    }

    pub fn generation(&self) -> u64 {
        lock(&self.inner.state).generation
    }
    pub fn is_closed(&self) -> bool {
        lock(&self.inner.state).closed
    }

    pub fn check(&self, cancellation: CancellationToken) -> Result<LintSnapshot, WakeError> {
        self.prepare_check(cancellation)?.run()
    }

    /// Admit synchronously before handing execution to another thread, preserving request order.
    pub fn prepare_check(&self, cancellation: CancellationToken) -> Result<LintCheck, WakeError> {
        cancellation.check()?;
        let (generation, documents) = {
            let mut state = lock(&self.inner.state);
            state.advance()?;
            state.active = Some(cancellation.clone());
            (state.generation, state.documents.clone())
        };
        Ok(LintCheck {
            inner: self.inner.clone(),
            generation,
            documents,
            cancellation,
        })
    }

    pub fn request_close(&self) {
        let mut state = lock(&self.inner.state);
        if !state.closed {
            state.generation = state.generation.saturating_add(1);
            state.closed = true;
            state.documents.clear();
            state.dependencies = Arc::default();
            if let Some(active) = state.active.take() {
                active.cancel();
            }
            self.inner.ready.notify_all();
        }
        drop(state);
        self.notify_changed();
    }

    pub fn close(&self) {
        self.request_close();
        let mut executing = lock(&self.inner.executing);
        while *executing {
            executing = self
                .inner
                .ready
                .wait(executing)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }
}

pub struct LintCheck {
    inner: Arc<Inner>,
    generation: u64,
    documents: BTreeMap<String, Arc<LintDocument>>,
    cancellation: CancellationToken,
}

impl LintCheck {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn run(self) -> Result<LintSnapshot, WakeError> {
        self.run_with(|| {})
    }

    fn run_with(self, before_analysis: impl FnOnce()) -> Result<LintSnapshot, WakeError> {
        let Self {
            inner,
            generation,
            documents,
            cancellation,
        } = self;
        let mut executing = lock(&inner.executing);
        while *executing {
            cancellation.check()?;
            executing = inner
                .ready
                .wait_timeout(executing, Duration::from_millis(10))
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        cancellation.check()?;
        *executing = true;
        drop(executing);
        let _execution = Execution(&inner);
        before_analysis();
        let mut dependencies = BTreeSet::new();
        let result = super::lint_project_observed(
            inner.options.clone(),
            &cancellation,
            &documents,
            Some(&inner.options.root),
            None,
            &mut dependencies,
        );
        let mut state = lock(&inner.state);
        if state.closed || state.generation != generation {
            return Err(WakeError::cancelled());
        }
        cancellation.check()?;
        state.active = None;
        state.dependencies = Arc::new(dependencies);
        Ok(LintSnapshot {
            generation,
            documents: documents
                .into_iter()
                .map(|(path, document)| (path, document.version))
                .collect(),
            result: result?,
        })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active) = state.active.take() {
            active.cancel();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_observations_replace_on_current_success_and_failure_only() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("wake.config.toml"),
            "[lint]\nrecommended=false\n[lint.rules]\n'import/no-unresolved'='error'",
        )
        .unwrap();
        std::fs::write(root.path().join("a.ts"), "import './missing';").unwrap();
        let context = LintContext::create(LintProjectOptions {
            root: root.path().into(),
            paths: vec!["a.ts".into()],
            ..Default::default()
        })
        .unwrap();
        context.check(CancellationToken::default()).unwrap();
        let first = context.watch_dependencies();
        assert!(first.contains(&wake_common::fs::normalize(&root.path().join("missing.ts"))));
        let obsolete = context.prepare_check(CancellationToken::default()).unwrap();
        context.invalidate().unwrap();
        assert_eq!(obsolete.run().unwrap_err().code, "WAKE_CANCELLED");
        assert_eq!(context.watch_dependencies(), first);
        std::fs::write(root.path().join("a.ts"), "import 'pkg';").unwrap();
        std::fs::write(root.path().join(".pnp.cjs"), "broken").unwrap();
        assert_eq!(
            context
                .check(CancellationToken::default())
                .unwrap_err()
                .code,
            "WAKE_LINT_ANALYSIS"
        );
        let failed = context.watch_dependencies();
        assert!(failed.contains(&wake_common::fs::normalize(&root.path().join(".pnp.cjs"))));
        assert!(!failed.contains(&wake_common::fs::normalize(&root.path().join("missing.ts"))));
        std::fs::write(
            root.path().join("wake.config.toml"),
            "[lint]\nrecommended=false",
        )
        .unwrap();
        context.check(CancellationToken::default()).unwrap();
        assert!(context.watch_dependencies().is_empty());
        context.close();
        assert!(context.watch_dependencies().is_empty());
    }

    #[test]
    fn close_waits_for_a_running_analysis_and_rejects_its_result() {
        let root = tempfile::tempdir().unwrap();
        let context = LintContext::create(LintProjectOptions {
            root: root.path().into(),
            ..Default::default()
        })
        .unwrap();
        let request = context.prepare_check(CancellationToken::default()).unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let running = std::thread::spawn(move || {
            request.run_with(|| {
                started.send(()).unwrap();
                wait.recv().unwrap();
            })
        });
        ready.recv().unwrap();
        context.request_close();
        let closer = context.clone();
        let (done, closed) = std::sync::mpsc::channel();
        let closing = std::thread::spawn(move || {
            closer.close();
            done.send(()).unwrap();
        });
        assert!(matches!(
            closed.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        release.send(()).unwrap();
        assert_eq!(running.join().unwrap().unwrap_err().code, "WAKE_CANCELLED");
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
        closing.join().unwrap();
    }
    #[test]
    fn newer_document_invalidates_running_snapshot_and_closed_context_waits_for_execution() {
        let root = tempfile::tempdir().unwrap();
        let context = LintContext::create(LintProjectOptions {
            root: root.path().into(),
            ..Default::default()
        })
        .unwrap();
        context
            .update_document(LintDocument {
                filename: "a.js".into(),
                version: 1,
                text: "debugger;".into(),
            })
            .unwrap();
        let (started, ready) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let worker = context.clone();
        let task = std::thread::spawn(move || {
            worker
                .prepare_check(CancellationToken::default())
                .unwrap()
                .run_with(|| {
                    started.send(()).unwrap();
                    wait.recv().unwrap();
                })
        });
        ready.recv().unwrap();
        context
            .update_document(LintDocument {
                filename: "a.js".into(),
                version: 2,
                text: "run();".into(),
            })
            .unwrap();
        release.send(()).unwrap();
        assert_eq!(task.join().unwrap().unwrap_err().code, "WAKE_CANCELLED");
        let current = context.check(CancellationToken::default()).unwrap();
        assert_eq!(current.documents["a.js"], 2);
        assert_eq!(current.result.error_count, 0);
        context.request_close();
        context.close();
        assert!(context.check(CancellationToken::default()).is_err());
    }
}
