//! Opt-in, build-scoped native diagnostics, independent of the Node event loop.
//!
//! Guards describe work, never success. Steps include their children; parallel totals
//! may exceed wall time. Context belongs only to active execution, never cached inputs,
//! recomputers, fingerprints, build results, or long-lived callbacks.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static ENABLED: AtomicBool = AtomicBool::new(false);
static ENV_ENABLED: OnceLock<bool> = OnceLock::new();
static SAMPLER: Mutex<Weak<Sampler>> = Mutex::new(Weak::new());
static NEXT_BUILD: AtomicU64 = AtomicU64::new(1);
const SLOW_LIMIT: usize = 10;
const ACTIVE_LIMIT: usize = 8;
type Sink = Arc<dyn Fn(String) + Send + Sync>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

thread_local! {
    static CURRENT: RefCell<ProgressContext> = RefCell::new(ProgressContext::default());
}

/// Enable process-wide diagnostics without mutating the environment.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// The environment is sampled once, on first use.
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
        || *ENV_ENABLED.get_or_init(|| std::env::var("WAKE_PROGRESS").is_ok_and(|v| v == "1"))
}

/// Execution context captured at task submission, not when defining cached work.
#[derive(Clone, Default)]
pub struct ProgressContext(Option<Context>);

#[derive(Clone)]
struct Context {
    build: Arc<Build>,
    parent: Option<u64>,
    module: Option<Arc<str>>,
}

impl ProgressContext {
    /// Capture the caller's build, parent activity and module.
    pub fn capture() -> Self {
        if enabled() {
            Self::current()
        } else {
            Self::default()
        }
    }

    fn current() -> Self {
        CURRENT.with(|current| current.borrow().clone())
    }

    /// Restore this context for a synchronous execution scope, including unwinding.
    /// Enter and drop scopes in stack order on the same thread.
    pub fn enter(&self) -> EnterGuard {
        if self.0.is_none() && !enabled() {
            return EnterGuard {
                previous: None,
                thread: PhantomData,
            };
        }
        let previous = CURRENT.with(|current| current.replace(self.clone()));
        EnterGuard {
            previous: Some(previous),
            thread: PhantomData,
        }
    }
}

/// A thread-local scope; use a cloned context, not this guard, to cross threads.
pub struct EnterGuard {
    previous: Option<ProgressContext>,
    thread: PhantomData<Rc<()>>,
}

impl Drop for EnterGuard {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            CURRENT.with(|current| {
                current.replace(previous);
            });
        }
    }
}

/// Start an independent build, even if the calling thread is inside another build.
pub fn build(name: &'static str, target: impl FnOnce() -> String) -> Span {
    start(name, target, Kind::Phase, true)
}

/// Track a phase. Without an inherited context this starts an independent build.
pub fn phase(name: &'static str, detail: impl FnOnce() -> String) -> Span {
    start(name, detail, Kind::Phase, false)
}

/// Track a module operation and include it in this build's bounded slowest list.
pub fn task(name: &'static str, module: impl FnOnce() -> String) -> Span {
    start(name, module, Kind::Task, false)
}

/// Track a fine-grained step, inheriting the current module. Names must be fixed
/// operation or pass names; put iteration numbers and other variable data in detail.
pub fn step(name: &'static str, detail: impl FnOnce() -> String) -> Span {
    start(name, detail, Kind::Step, false)
}

fn start(
    name: &'static str,
    detail: impl FnOnce() -> String,
    kind: Kind,
    independent: bool,
) -> Span {
    if !enabled() {
        return Span {
            registration: None,
            entered: None,
        };
    }
    let detail = clean(&detail());
    let context = if independent {
        None
    } else {
        ProgressContext::current().0
    };
    let (build, parent, module) = match context {
        Some(context) => (context.build, context.parent, context.module),
        None => (
            Build::new(
                shared_sampler(),
                name,
                detail.clone(),
                Arc::new(Instant::now),
            ),
            None,
            None,
        ),
    };
    build.start(name, detail, kind, parent, module)
}

/// RAII activity and context scope. Drop in stack order on the creating thread.
#[must_use = "keep the progress guard alive for the observed operation"]
pub struct Span {
    registration: Option<(Arc<Build>, u64)>,
    entered: Option<EnterGuard>,
}

impl Span {
    /// Refine the root build's target after configuration/entry resolution.
    /// Disabled diagnostics do not evaluate the closure.
    pub fn set_target(&self, target: impl FnOnce() -> String) {
        if let Some((build, 1)) = &self.registration {
            let target = clean(&target());
            let mut state = build.state.lock().unwrap_or_else(|e| e.into_inner());
            state.target = target.clone();
            if let Some(root) = state.active.get_mut(&1) {
                root.detail = target;
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Phase,
    Task,
    Step,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Phase => "phase",
            Self::Task => "module",
            Self::Step => "step",
        }
    }
}

struct Activity {
    name: &'static str,
    detail: String,
    module: Option<Arc<str>>,
    parent: Option<u64>,
    started: Instant,
    kind: Kind,
}

struct Finished {
    name: &'static str,
    detail: String,
    elapsed: Duration,
    id: u64,
}

#[derive(Default)]
struct Aggregate {
    count: u64,
    total: Duration,
    max: Duration,
}

struct State {
    name: &'static str,
    target: String,
    next_id: u64,
    active: BTreeMap<u64, Activity>,
    groups: BTreeMap<(Kind, &'static str), Aggregate>,
    slowest: Vec<Finished>,
    finished: u64,
}

struct Build {
    id: u64,
    started: Instant,
    state: Arc<Mutex<State>>,
    sampler: Arc<Sampler>,
    clock: Clock,
}

struct Sampling {
    builds: Mutex<BTreeMap<u64, Weak<Mutex<State>>>>,
    emission: Mutex<()>,
    sink: Sink,
}

struct Sampler {
    shared: Arc<Sampling>,
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

fn shared_sampler() -> Arc<Sampler> {
    let mut shared = SAMPLER.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sampler) = shared.upgrade() {
        return sampler;
    }
    let sampler = Sampler::new(
        Duration::from_secs(1),
        Arc::new(|line| {
            // Closed stderr is an observation failure, not a build failure.
            let _ = writeln!(std::io::stderr().lock(), "{line}");
        }),
    );
    *shared = Arc::downgrade(&sampler);
    sampler
}

fn clean(detail: &str) -> String {
    let mut result = String::new();
    for (index, ch) in detail.chars().enumerate() {
        if index == 512 {
            result.push('…');
            break;
        }
        if ch.is_control() {
            result.extend(ch.escape_default());
        } else {
            result.push(ch);
        }
    }
    result
}

fn line(build: u64, text: impl std::fmt::Display) -> String {
    format!("[wake-progress #{build}] {text}")
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl Sampler {
    fn new(interval: Duration, sink: Sink) -> Arc<Self> {
        let shared = Arc::new(Sampling {
            builds: Mutex::new(BTreeMap::new()),
            emission: Mutex::new(()),
            sink,
        });
        let (stop, receiver) = mpsc::channel();
        let observed = shared.clone();
        // The worker owns only sampling state, never a Build or Sampler. It cannot
        // accidentally become the last owner and try joining itself.
        let worker = std::thread::Builder::new()
            .name("wake-progress".into())
            .spawn(move || {
                while matches!(
                    receiver.recv_timeout(interval),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    observed.sample(Instant::now());
                }
            });
        let worker = match worker {
            Ok(worker) => Some(worker),
            Err(error) => {
                (shared.sink)(format!("[wake-progress] heartbeat unavailable: {error}"));
                None
            }
        };
        Arc::new(Self {
            shared,
            stop,
            worker,
        })
    }
}

impl Sampling {
    fn sample(&self, now: Instant) {
        // Serialize summaries with sampling so no heartbeat follows a build's summary.
        let _emission = self.emission.lock().unwrap_or_else(|e| e.into_inner());
        let states = self
            .builds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter_map(|(id, state)| state.upgrade().map(|state| (*id, state)))
            .collect::<Vec<_>>();
        for (id, state) in states {
            let rows = state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .heartbeat(id, now);
            for row in rows {
                (self.sink)(row);
            }
        }
    }
}

impl Build {
    fn new(sampler: Arc<Sampler>, name: &'static str, target: String, clock: Clock) -> Arc<Self> {
        let id = NEXT_BUILD.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(Mutex::new(State {
            name,
            target,
            next_id: 0,
            active: BTreeMap::new(),
            groups: BTreeMap::new(),
            slowest: Vec::new(),
            finished: 0,
        }));
        sampler
            .shared
            .builds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::downgrade(&state));
        Arc::new(Self {
            id,
            started: clock(),
            state,
            sampler,
            clock,
        })
    }

    fn start(
        self: &Arc<Self>,
        name: &'static str,
        detail: String,
        kind: Kind,
        parent: Option<u64>,
        inherited_module: Option<Arc<str>>,
    ) -> Span {
        let module = if kind == Kind::Task {
            Some(Arc::from(detail.as_str()))
        } else {
            inherited_module
        };
        let id = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.next_id += 1;
            let id = state.next_id;
            state.active.insert(
                id,
                Activity {
                    name,
                    detail: detail.clone(),
                    module: module.clone(),
                    parent,
                    kind,
                    started: (self.clock)(),
                },
            );
            id
        };
        if kind == Kind::Phase {
            (self.sampler.shared.sink)(line(
                self.id,
                format!("begin #{id} {name} parent={parent:?} {detail}"),
            ));
        }
        let context = ProgressContext(Some(Context {
            build: self.clone(),
            parent: Some(id),
            module,
        }));
        Span {
            registration: Some((self.clone(), id)),
            entered: Some(context.enter()),
        }
    }
}

impl State {
    fn heartbeat(&self, build: u64, now: Instant) -> Vec<String> {
        let phase = self
            .active
            .values()
            .rev()
            .find(|a| a.kind == Kind::Phase)
            .map_or(self.name, |a| a.name);
        let mut rows = vec![line(
            build,
            format!(
                "build {} target={} phase={phase} active={} finished={}",
                self.name,
                self.target,
                self.active.len(),
                self.finished
            ),
        )];
        let mut active = self.active.iter().collect::<Vec<_>>();
        // Prefer leaves so iteration scopes cannot crowd the actual executing passes out.
        let parents = self
            .active
            .values()
            .filter_map(|a| a.parent)
            .collect::<BTreeSet<_>>();
        active.sort_by_key(|(id, a)| (parents.contains(id), std::cmp::Reverse(a.kind), a.started));
        for (id, activity) in active.into_iter().take(ACTIVE_LIMIT) {
            rows.push(line(
                build,
                format!(
                    "active #{id} {} {:.1}ms parent={:?} {} {}",
                    activity.name,
                    millis(now.saturating_duration_since(activity.started)),
                    activity.parent,
                    if activity.kind == Kind::Step {
                        activity.module.as_deref().unwrap_or("")
                    } else {
                        ""
                    },
                    activity.detail
                ),
            ));
        }
        rows
    }

    fn finish(&mut self, build: u64, id: u64, now: Instant) -> Option<String> {
        let activity = self.active.remove(&id)?;
        let elapsed = now.saturating_duration_since(activity.started);
        self.finished += 1;
        let aggregate = self
            .groups
            .entry((activity.kind, activity.name))
            .or_default();
        aggregate.count += 1;
        aggregate.total += elapsed;
        aggregate.max = aggregate.max.max(elapsed);
        if activity.kind == Kind::Task {
            self.slowest.push(Finished {
                name: activity.name,
                detail: activity.detail,
                elapsed,
                id,
            });
            self.slowest
                .sort_by_key(|item| std::cmp::Reverse(item.elapsed));
            self.slowest.truncate(SLOW_LIMIT);
            None
        } else if activity.kind == Kind::Phase {
            Some(line(
                build,
                format!(
                    "end #{id} {} {:.1}ms {}",
                    activity.name,
                    millis(elapsed),
                    activity.detail
                ),
            ))
        } else {
            None
        }
    }

    fn summary(&self, build: u64, elapsed: Duration) -> Vec<String> {
        let mut rows = vec![line(
            build,
            format!(
                "summary {} target={} wall={:.1}ms operations={} (inclusive steps; parallel totals may exceed wall time; do not add rows)",
                self.name,
                self.target,
                millis(elapsed),
                self.finished
            ),
        )];
        let mut groups = self.groups.iter().collect::<Vec<_>>();
        groups.sort_by_key(|(key, value)| (std::cmp::Reverse(value.total), **key));
        for ((kind, name), value) in groups {
            rows.push(line(
                build,
                format!(
                    "{} {name} count={} total={:.1}ms avg={:.1}ms max={:.1}ms",
                    kind.label(),
                    value.count,
                    millis(value.total),
                    millis(value.total) / value.count as f64,
                    millis(value.max)
                ),
            ));
        }
        for item in &self.slowest {
            rows.push(line(
                build,
                format!(
                    "slowest #{} {} {:.1}ms {}",
                    item.id,
                    item.name,
                    millis(item.elapsed),
                    item.detail
                ),
            ));
        }
        rows.push(line(
            build,
            "build ended (scope exit, not a success status)",
        ));
        rows
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // Restore the parent before releasing this registration's last build owner.
        drop(self.entered.take());
        if let Some((build, id)) = self.registration.take() {
            let end = build
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .finish(build.id, id, (build.clock)());
            if let Some(end) = end {
                (build.sampler.shared.sink)(end);
            }
        }
    }
}

impl Drop for Build {
    fn drop(&mut self) {
        let _emission = self
            .sampler
            .shared
            .emission
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.sampler
            .shared
            .builds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        let rows = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .summary(
                self.id,
                (self.clock)().saturating_duration_since(self.started),
            );
        for row in rows {
            (self.sampler.shared.sink)(row);
        }
    }
}

impl Drop for Sampler {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        sampler: Arc<Sampler>,
        output: Arc<Mutex<Vec<String>>>,
        tick: Arc<AtomicU64>,
        clock: Clock,
    }

    impl Fixture {
        fn new() -> Self {
            let output = Arc::new(Mutex::new(Vec::new()));
            let sink = output.clone();
            let sampler = Sampler::new(
                Duration::from_secs(3600),
                Arc::new(move |line| sink.lock().unwrap().push(line)),
            );
            let tick = Arc::new(AtomicU64::new(0));
            let time = tick.clone();
            let base = Instant::now();
            let clock: Clock =
                Arc::new(move || base + Duration::from_millis(time.load(Ordering::Relaxed)));
            Self {
                sampler,
                output,
                tick,
                clock,
            }
        }
        fn build(&self, target: &str) -> Arc<Build> {
            Build::new(
                self.sampler.clone(),
                "build",
                target.into(),
                self.clock.clone(),
            )
        }
        fn advance(&self, ms: u64) {
            self.tick.fetch_add(ms, Ordering::Relaxed);
        }
        fn text(&self) -> String {
            self.output.lock().unwrap().join("\n")
        }
    }

    #[test]
    fn aggregates_all_short_tasks_with_bounded_groups_and_top_ten() {
        let f = Fixture::new();
        let build = f.build("entry.js");
        let root = build.start("scan", String::new(), Kind::Phase, None, None);
        root.set_target(|| "resolved/entry.js".into());
        for duration in [10, 20, 30] {
            let task = build.start("parse", format!("{duration}.js"), Kind::Task, Some(1), None);
            f.advance(duration);
            drop(task);
        }
        for i in 0..1000 {
            let task = build.start("read", format!("{i}.js"), Kind::Task, Some(1), None);
            f.advance(1);
            drop(task);
        }
        drop(root);
        {
            let state = build.state.lock().unwrap();
            assert!(state.active.is_empty());
            assert_eq!(state.groups.len(), 3);
            assert_eq!(state.slowest.len(), 10);
        }
        drop(build);
        let output = f.text();
        assert!(output.contains("wall=1060.0ms"));
        assert!(output.contains("target=resolved/entry.js"));
        assert!(output.contains("parse count=3 total=60.0ms avg=20.0ms max=30.0ms"));
        assert!(output.contains("read count=1000 total=1000.0ms avg=1.0ms max=1.0ms"));
        assert!(output.find("read count=").unwrap() < output.find("parse count=").unwrap());
        assert!(f.sampler.shared.builds.lock().unwrap().is_empty());
    }

    #[test]
    fn context_crosses_threads_and_restores_after_unwinding() {
        let f = Fixture::new();
        let build = f.build("first.js");
        let task = build.start("optimize", "first.js".into(), Kind::Task, None, None);
        let context = ProgressContext::current();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let other = f.build("other.js");
            let _phase = other.start("scan", String::new(), Kind::Phase, None, None);
            panic!("controlled unwind");
        }));
        assert_eq!(ProgressContext::current().0.unwrap().build.id, build.id);
        let (ready, wait) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _entered = context.enter();
            let inherited = ProgressContext::current().0.unwrap();
            let _step = inherited.build.start(
                "fold",
                "iteration=2".into(),
                Kind::Step,
                inherited.parent,
                inherited.module,
            );
            ready.send(()).unwrap();
            blocked.recv().unwrap();
        });
        wait.recv_timeout(Duration::from_secs(5)).unwrap();
        f.advance(5000);
        f.sampler.shared.sample((f.clock)());
        let output = f.text();
        assert!(output.contains("fold 5000.0ms parent=Some(1) first.js iteration=2"));
        release.send(()).unwrap();
        worker.join().unwrap();
        drop(task);
        drop(build);
        assert!(ProgressContext::current().0.is_none());
        assert!(f.sampler.shared.builds.lock().unwrap().is_empty());
    }

    #[test]
    fn parallel_inclusive_totals_and_independent_summaries() {
        let f = Fixture::new();
        let first = f.build("first.js");
        let root = first.start("build", String::new(), Kind::Phase, None, None);
        // Register concurrent operations explicitly; execution scopes remain stack ordered.
        let one = first.start("parse", "one.js".into(), Kind::Task, Some(1), None);
        let two = first.start("parse", "two.js".into(), Kind::Task, Some(1), None);
        f.advance(10);
        drop(two);
        drop(one);
        let second = f.build("second.js");
        let other = second.start("read", "second.js".into(), Kind::Task, None, None);
        f.advance(5);
        drop(other);
        drop(second);
        assert!(f.text().contains("target=second.js wall=5.0ms"));
        assert!(!f.text().contains("target=first.js wall="));
        assert_eq!(f.sampler.shared.builds.lock().unwrap().len(), 1);
        drop(root);
        drop(first);
        let output = f.text();
        assert!(output.contains("target=first.js wall=15.0ms"));
        assert!(output.contains("parse count=2 total=20.0ms avg=10.0ms max=10.0ms"));
        assert!(output.contains("inclusive steps; parallel totals"));
    }

    #[test]
    fn phase_is_visible_beside_bounded_details() {
        assert_eq!(clean("a\n\x1b\r\tb"), "a\\n\\u{1b}\\r\\tb");
        assert_eq!(clean(&"a".repeat(600)).chars().count(), 513);
        let f = Fixture::new();
        let build = f.build("entry.js");
        let root = build.start("scan", String::new(), Kind::Phase, None, None);
        let mut tasks = (0..20)
            .map(|i| build.start("read", format!("{i}.js"), Kind::Task, Some(1), None))
            .collect::<Vec<_>>();
        let rows = build.state.lock().unwrap().heartbeat(build.id, (f.clock)());
        assert_eq!(rows.len(), ACTIVE_LIMIT + 1);
        assert!(rows[0].contains("target=entry.js phase=scan active=21"));
        while tasks.pop().is_some() {}
        drop(root);
    }

    #[test]
    fn disabled_child() {
        if std::env::var_os("WAKE_PROGRESS_DISABLED_CHILD").is_none() {
            return;
        }
        assert!(!enabled());
        let before = NEXT_BUILD.load(Ordering::Relaxed);
        let _build = build("build", || panic!("disabled target evaluated"));
        _build.set_target(|| panic!("disabled resolved target evaluated"));
        let _phase = phase("scan", || panic!("disabled phase detail evaluated"));
        let _task = task("parse", || panic!("disabled path evaluated"));
        let _step = step("fold", || panic!("disabled iteration evaluated"));
        let _context = ProgressContext::capture().enter();
        assert!(ProgressContext::current().0.is_none());
        assert!(SAMPLER.lock().unwrap().upgrade().is_none());
        assert_eq!(NEXT_BUILD.load(Ordering::Relaxed), before);
    }

    #[test]
    fn disabled_diagnostics_do_not_register_or_evaluate_details() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "progress::tests::disabled_child", "--nocapture"])
            .env_remove("WAKE_PROGRESS")
            .env("WAKE_PROGRESS_DISABLED_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stderr).contains("[wake-progress"));
    }

    #[test]
    fn last_build_stops_sampling_and_summary_is_last() {
        let (tx, rx) = mpsc::channel();
        let sampler = Sampler::new(
            Duration::from_millis(10),
            Arc::new(move |line| {
                let _ = tx.send(line);
            }),
        );
        let build = Build::new(
            sampler.clone(),
            "build",
            "entry.js".into(),
            Arc::new(Instant::now),
        );
        let task = build.start("read", "entry.js".into(), Kind::Task, None, None);
        let mut sampled = false;
        while !sampled {
            sampled = rx
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .contains("active #");
        }
        drop(sampler);
        drop(task);
        drop(build);
        let rows = rx.iter().collect::<Vec<_>>();
        assert!(rows.last().unwrap().contains("build ended"));
    }
}
