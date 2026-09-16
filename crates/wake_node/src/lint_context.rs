//! A bounded pull-based adapter: background threads never call a possibly retired JS environment.
use serde_json::json;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use wake_app::{CancellationToken, LintCheck, LintContext, WakeError};

#[derive(Default)]
struct State {
    pending: Option<(u64, LintCheck)>,
    completed: Option<(u64, Result<String, WakeError>)>,
    closed: bool,
}
struct Worker {
    state: Mutex<State>,
    ready: Condvar,
}
pub(super) struct LintContextResource {
    pub context: LintContext,
    worker: Arc<Worker>,
    join: Mutex<Option<JoinHandle<()>>>,
    watch: Mutex<Option<wake_app::LintWatcher>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl LintContextResource {
    pub fn new(context: LintContext) -> Result<Self, WakeError> {
        let worker = Arc::new(Worker {
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
        });
        let background = worker.clone();
        let join = thread::Builder::new()
            .name("wake-node-lint".into())
            .spawn(move || {
                loop {
                    let (generation, request) = {
                        let mut state = lock(&background.state);
                        while !state.closed && state.pending.is_none() {
                            state = background
                                .ready
                                .wait(state)
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                        }
                        if state.closed {
                            break;
                        }
                        state.pending.take().expect("pending request")
                    };
                    let result =
                        catch_unwind(AssertUnwindSafe(|| request.run())).unwrap_or_else(|_| {
                            Err(WakeError::new("WAKE_INTERNAL", "Lint analysis panicked"))
                        });
                    let envelope = match result {
                        Ok(snapshot) => json!({"ok":true, "value":snapshot}),
                        Err(error) => json!({"ok":false, "error":error}),
                    };
                    let result = serde_json::to_string(&envelope)
                        .map_err(|e| WakeError::new("WAKE_INTERNAL", e.to_string()));
                    let mut state = lock(&background.state);
                    if !state.closed {
                        state.completed = Some((generation, result));
                    }
                }
            })
            .map_err(|e| WakeError::new("WAKE_INTERNAL", e.to_string()))?;
        Ok(Self {
            context,
            worker,
            join: Mutex::new(Some(join)),
            watch: Mutex::new(None),
        })
    }

    pub fn start(&self) -> Result<u64, WakeError> {
        let request = self.context.prepare_check(CancellationToken::default())?;
        let generation = request.generation();
        let mut state = lock(&self.worker.state);
        if state.closed {
            return Err(WakeError::closed("LintContext"));
        }
        state.pending = Some((generation, request));
        state.completed = None;
        self.worker.ready.notify_one();
        Ok(generation)
    }

    pub fn poll(&self, generation: f64) -> Result<Option<String>, WakeError> {
        if self.context.is_closed() || generation != self.context.generation() as f64 {
            return Err(WakeError::cancelled());
        }
        let mut state = lock(&self.worker.state);
        if state
            .completed
            .as_ref()
            .is_some_and(|(id, _)| *id as f64 == generation)
        {
            return state
                .completed
                .take()
                .expect("matched completion")
                .1
                .map(Some);
        }
        Ok(None)
    }

    pub fn cancel(&self, generation: f64) -> Result<(), WakeError> {
        if !self.context.is_closed() && generation == self.context.generation() as f64 {
            self.context.invalidate()?;
        }
        Ok(())
    }

    pub fn request_close(&self) {
        self.context.request_close();
        self.stop_watch();
        let mut state = lock(&self.worker.state);
        state.closed = true;
        state.pending = None;
        state.completed = None;
        self.worker.ready.notify_all();
    }

    pub fn poll_closed(&self) -> bool {
        if !self.poll_watch_stopped() {
            return false;
        }
        let mut join = lock(&self.join);
        if join.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        if let Some(join) = join.take() {
            let _ = join.join();
        }
        true
    }

    pub fn close(&self) {
        self.request_close();
        if let Some(watch) = lock(&self.watch).take() {
            watch.stop();
        }
        if let Some(join) = lock(&self.join).take() {
            let _ = join.join();
        }
    }

    pub fn start_watch(&self) -> Result<(), WakeError> {
        let mut watch = lock(&self.watch);
        if let Some(current) = watch.as_ref() {
            if current.is_watching() {
                return Ok(());
            }
            if !current.poll_stopped() {
                return Err(WakeError::new(
                    "WAKE_LINT_WATCH",
                    "Previous lint watcher is still stopping",
                ));
            }
        }
        *watch = Some(wake_app::LintWatcher::start(self.context.clone())?);
        Ok(())
    }

    pub fn stop_watch(&self) {
        if let Some(watch) = lock(&self.watch).as_ref() {
            watch.request_stop();
        }
    }
    pub fn is_watching(&self) -> bool {
        lock(&self.watch)
            .as_ref()
            .is_some_and(wake_app::LintWatcher::is_watching)
    }
    pub fn drain_watch(&self) -> Vec<wake_app::LintWatchEvent> {
        lock(&self.watch)
            .as_ref()
            .map(wake_app::LintWatcher::drain_events)
            .unwrap_or_default()
    }
    pub fn poll_watch_stopped(&self) -> bool {
        let mut watch = lock(&self.watch);
        if watch.as_ref().is_some_and(|watch| !watch.poll_stopped()) {
            return false;
        }
        *watch = None;
        true
    }
}

impl Drop for LintContextResource {
    fn drop(&mut self) {
        self.close();
    }
}

pub(super) struct LintTaskResource {
    cancellation: CancellationToken,
    cancelled: AtomicBool,
    result: Arc<Mutex<Option<Result<String, WakeError>>>>,
    join: Mutex<Option<JoinHandle<()>>>,
}

impl LintTaskResource {
    pub fn new(options: wake_app::LintProjectOptions) -> Result<Self, WakeError> {
        let cancellation = CancellationToken::default();
        let result = Arc::new(Mutex::new(None));
        let completed = result.clone();
        let token = cancellation.clone();
        let join = thread::Builder::new()
            .name("wake-node-lint-once".into())
            .spawn(move || {
                let result =
                    catch_unwind(AssertUnwindSafe(|| wake_app::lint_project(options, &token)))
                        .unwrap_or_else(|_| {
                            Err(WakeError::new("WAKE_INTERNAL", "Lint analysis panicked"))
                        });
                let envelope = match result {
                    Ok(value) => json!({"ok":true, "value":value}),
                    Err(error) => json!({"ok":false, "error":error}),
                };
                *lock(&completed) = Some(
                    serde_json::to_string(&envelope)
                        .map_err(|e| WakeError::new("WAKE_INTERNAL", e.to_string())),
                );
            })
            .map_err(|e| WakeError::new("WAKE_INTERNAL", e.to_string()))?;
        Ok(Self {
            cancellation,
            cancelled: AtomicBool::new(false),
            result,
            join: Mutex::new(Some(join)),
        })
    }

    pub fn poll(&self) -> Result<Option<String>, WakeError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(WakeError::cancelled());
        }
        lock(&self.result).take().transpose()
    }

    pub fn cancel(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.cancellation.cancel();
        }
    }

    pub fn poll_closed(&self) -> bool {
        let mut join = lock(&self.join);
        if join.as_ref().is_some_and(|task| !task.is_finished()) {
            return false;
        }
        if let Some(join) = join.take() {
            let _ = join.join();
        }
        true
    }

    pub fn close(&self) {
        self.cancel();
        if let Some(join) = lock(&self.join).take() {
            let _ = join.join();
        }
    }
}

impl Drop for LintTaskResource {
    fn drop(&mut self) {
        self.close();
    }
}
