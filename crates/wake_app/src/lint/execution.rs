//! Product-owned batching of independent file work. Publication remains on the coordinator.
use crate::{CancellationToken, WakeError};
use std::cell::Cell;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use wake_turbo::Executor;

pub(super) struct Scheduler {
    executor: Arc<Executor>,
    active: Mutex<usize>,
    ready: Condvar,
}

pub(super) fn global() -> &'static Scheduler {
    static SCHEDULER: OnceLock<Scheduler> = OnceLock::new();
    SCHEDULER.get_or_init(|| Scheduler::new(wake_turbo::global_executor()))
}

impl Scheduler {
    pub fn new(executor: Arc<Executor>) -> Self {
        Self {
            executor,
            active: Mutex::new(0),
            ready: Condvar::new(),
        }
    }
    pub fn batch_size(&self) -> usize {
        self.executor.num_threads().min(32)
    }

    pub fn admit(&self, cancellation: &CancellationToken) -> Result<Batch<'_>, WakeError> {
        let mut active = self.active.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            cancellation.check()?;
            if *active < 2 {
                *active += 1;
                return Ok(Batch(self, PhantomData));
            }
            active = self
                .ready
                .wait_timeout(active, Duration::from_millis(10))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    pub fn run<T, F>(
        &self,
        jobs: Vec<F>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Result<T, WakeError>>, WakeError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, WakeError> + Send + 'static,
    {
        self.admit(cancellation)?.run(jobs, cancellation)
    }
}

// A coordinator can move its admission, but cannot share it between simultaneous windows.
pub(super) struct Batch<'a>(&'a Scheduler, PhantomData<Cell<()>>);
impl Batch<'_> {
    pub fn batch_size(&self) -> usize {
        self.0.batch_size()
    }

    pub fn run<T, F>(
        self,
        jobs: Vec<F>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Result<T, WakeError>>, WakeError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, WakeError> + Send + 'static,
    {
        self.run_window(jobs, cancellation)
    }

    pub fn run_window<T, F>(
        &self,
        jobs: Vec<F>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<Result<T, WakeError>>, WakeError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, WakeError> + Send + 'static,
    {
        cancellation.check()?;
        if jobs.len() > self.0.batch_size() {
            return Err(WakeError::new(
                "WAKE_INTERNAL",
                "Lint batch exceeds its execution window",
            ));
        }
        let jobs = jobs
            .into_iter()
            .map(|job| {
                let cancellation = cancellation.clone();
                move || {
                    cancellation.check()?;
                    let result = catch_unwind(AssertUnwindSafe(job)).unwrap_or_else(|_| {
                        Err(WakeError::new(
                            "WAKE_INTERNAL",
                            "Lint file analysis panicked",
                        ))
                    });
                    cancellation.check()?;
                    result
                }
            })
            .collect();
        let output = self.0.executor.parallel(jobs);
        cancellation.check()?;
        Ok(output)
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        *self.0.active.lock().unwrap_or_else(|p| p.into_inner()) -= 1;
        self.0.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::{Condvar, Mutex, mpsc};
    use std::time::{Duration, Instant};

    #[test]
    fn retained_admission_runs_windows_without_admitting_a_third_project() {
        let scheduler = Arc::new(Scheduler::new(Arc::new(Executor::new(2))));
        let cancellation = CancellationToken::default();
        let first = scheduler.admit(&cancellation).unwrap();
        let second = scheduler.admit(&cancellation).unwrap();
        for window in 0..3 {
            let jobs = (0..2).map(|index| move || Ok(window * 2 + index)).collect();
            let result = first.run_window(jobs, &cancellation).unwrap();
            assert_eq!(
                result.into_iter().map(Result::unwrap).collect::<Vec<_>>(),
                [window * 2, window * 2 + 1]
            );
        }
        let queued_scheduler = scheduler.clone();
        let queued_cancellation = CancellationToken::default();
        let cancel = queued_cancellation.clone();
        let (sent, received) = mpsc::channel();
        let queued = std::thread::spawn(move || {
            let result = queued_scheduler.run(vec![|| Ok(7)], &queued_cancellation);
            sent.send(result).unwrap();
        });
        assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
        cancel.cancel();
        assert_eq!(
            received
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap_err()
                .code,
            "WAKE_CANCELLED"
        );
        queued.join().unwrap();
        let stopped = CancellationToken::default();
        stopped.cancel();
        assert_eq!(
            first.run_window(vec![|| Ok(8)], &stopped).unwrap_err().code,
            "WAKE_CANCELLED"
        );
        drop(second);
        assert_eq!(
            *scheduler.run(vec![|| Ok(9)], &cancellation).unwrap()[0]
                .as_ref()
                .unwrap(),
            9
        );
        assert_eq!(
            *first.run_window(vec![|| Ok(10)], &cancellation).unwrap()[0]
                .as_ref()
                .unwrap(),
            10
        );
    }

    #[test]
    fn independent_files_overlap_on_the_shared_pool_and_results_keep_input_order() {
        let scheduler = Scheduler::new(Arc::new(Executor::new(4)));
        let arrived = Arc::new((Mutex::new(0), Condvar::new()));
        let jobs = (0..4)
            .map(|index| {
                let arrived = arrived.clone();
                move || {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    let mut count = arrived.0.lock().unwrap();
                    *count += 1;
                    arrived.1.notify_all();
                    while *count < 4 && Instant::now() < deadline {
                        count = arrived
                            .1
                            .wait_timeout(count, Duration::from_millis(10))
                            .unwrap()
                            .0;
                    }
                    Ok((
                        index,
                        *count == 4,
                        std::thread::current().name().unwrap_or("").to_owned(),
                    ))
                }
            })
            .collect();
        let results = scheduler.run(jobs, &CancellationToken::default()).unwrap();
        let values: Vec<_> = results.into_iter().map(Result::unwrap).collect();
        assert!(
            values.iter().all(|value| value.1),
            "independent files never overlapped: {values:?}"
        );
        assert_eq!(
            values.iter().map(|value| value.0).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        assert_eq!(
            values
                .iter()
                .map(|value| &value.2)
                .collect::<BTreeSet<_>>()
                .len(),
            4
        );
    }

    #[test]
    fn panicking_file_does_not_destroy_workers_or_hide_other_file_results() {
        let scheduler = Scheduler::new(Arc::new(Executor::new(2)));
        let jobs: Vec<Box<dyn FnOnce() -> Result<usize, WakeError> + Send>> = vec![
            Box::new(|| panic!("controlled lint task failure")),
            Box::new(|| Ok(2)),
        ];
        let results = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            scheduler.run(jobs, &CancellationToken::default())
        }));
        assert!(results.is_ok(), "file panic escaped the executor boundary");
        let values = results.unwrap().unwrap();
        assert_eq!(values[0].as_ref().unwrap_err().code, "WAKE_INTERNAL");
        assert_eq!(*values[1].as_ref().unwrap(), 2);
        assert_eq!(
            scheduler
                .run(vec![|| Ok(3)], &CancellationToken::default())
                .unwrap()[0]
                .as_ref()
                .unwrap(),
            &3
        );
    }

    #[test]
    fn all_projects_share_two_batch_slots_and_a_waiting_request_can_cancel() {
        let scheduler = Arc::new(Scheduler::new(Arc::new(Executor::new(4))));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let mut joins = Vec::new();
        for _ in 0..2 {
            let scheduler = scheduler.clone();
            let release = release.clone();
            let started = started.clone();
            joins.push(std::thread::spawn(move || {
                scheduler.run(
                    vec![move || {
                        started.send(()).unwrap();
                        let gate = release.0.lock().unwrap();
                        let (gate, _) = release
                            .1
                            .wait_timeout_while(gate, Duration::from_secs(3), |open| !*open)
                            .unwrap();
                        assert!(*gate, "test did not release running batches");
                        Ok(())
                    }],
                    &CancellationToken::default(),
                )
            }));
        }
        for _ in 0..2 {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let cancelled = CancellationToken::default();
        let cancellation = cancelled.clone();
        let queued_scheduler = scheduler.clone();
        let (completed, completion) = mpsc::channel();
        let queued = std::thread::spawn(move || {
            let result = queued_scheduler.run(
                vec![move || {
                    started.send(()).unwrap();
                    Ok(())
                }],
                &cancellation,
            );
            completed.send(result).unwrap();
        });
        let admitted_extra = received.recv_timeout(Duration::from_millis(100)).is_ok();
        cancelled.cancel();
        let cancelled_result = completion.recv_timeout(Duration::from_secs(1));
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        for join in joins {
            assert!(join.join().unwrap().is_ok());
        }
        queued.join().unwrap();
        assert!(
            !admitted_extra,
            "third project bypassed the process batch limit"
        );
        assert_eq!(
            cancelled_result.unwrap().unwrap_err().code,
            "WAKE_CANCELLED"
        );
        assert!(
            scheduler
                .run(vec![|| Ok(())], &CancellationToken::default())
                .is_ok()
        );
    }

    #[test]
    fn cancellation_skips_files_already_queued_behind_busy_workers() {
        let scheduler = Arc::new(Scheduler::new(Arc::new(Executor::new(2))));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (started, received) = mpsc::channel();
        let busy_scheduler = scheduler.clone();
        let jobs = (0..2)
            .map(|_| {
                let release = release.clone();
                let started = started.clone();
                move || {
                    started.send(()).unwrap();
                    let gate = release.0.lock().unwrap();
                    let (gate, _) = release
                        .1
                        .wait_timeout_while(gate, Duration::from_secs(3), |open| !*open)
                        .unwrap();
                    assert!(*gate);
                    Ok(())
                }
            })
            .collect();
        let busy =
            std::thread::spawn(move || busy_scheduler.run(jobs, &CancellationToken::default()));
        for _ in 0..2 {
            received.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let token = CancellationToken::default();
        let cancellation = token.clone();
        let queued_scheduler = scheduler.clone();
        let queued = std::thread::spawn(move || {
            queued_scheduler.run(
                vec![move || {
                    started.send(()).unwrap();
                    Ok(())
                }],
                &cancellation,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while *scheduler.active.lock().unwrap() != 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        let admitted = *scheduler.active.lock().unwrap() == 2;
        token.cancel();
        *release.0.lock().unwrap() = true;
        release.1.notify_all();
        assert!(busy.join().unwrap().is_ok());
        assert_eq!(queued.join().unwrap().unwrap_err().code, "WAKE_CANCELLED");
        assert!(admitted, "queued batch never acquired admission");
        assert!(
            received.try_recv().is_err(),
            "cancelled queued file still ran"
        );
    }
}
