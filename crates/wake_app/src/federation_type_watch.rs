//! Development-only control-plane monitor for remote declaration builds.
//!
//! The monitor probes only manifests opted into `devFollow`. A changed manifest revision triggers
//! the existing all-remotes fail-closed synchronizer, so this module never owns declaration
//! validation or publication semantics.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use wake_federation_contract::{FederationConfig, FederationLock};

use super::WakeError;
use super::federation_type_sync::{
    FederationTypeRevisions, FederationTypeSyncResult, followed_type_revisions,
    probe_followed_federation_type_revisions, sync_federation_types_for_development,
};

const TYPE_POLL_INTERVAL: Duration = Duration::from_secs(1);

type Probe = Arc<dyn Fn() -> Result<FederationTypeRevisions, WakeError> + Send + Sync + 'static>;
type Synchronize =
    Arc<dyn Fn() -> Result<FederationTypeRevisions, WakeError> + Send + Sync + 'static>;
type ReportError = Arc<dyn Fn(WakeError) + Send + Sync + 'static>;

/// Shared lifecycle guard held by every clone of an application development server.
#[derive(Clone)]
pub(crate) struct FederationTypeMonitor {
    inner: Arc<MonitorInner>,
}

struct MonitorInner {
    stop: Arc<StopSignal>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl FederationTypeMonitor {
    /// Prevent another probe or synchronization pass from starting.
    pub(crate) fn request_stop(&self) {
        self.inner.stop.request();
    }

    /// Stop and join the monitor. After this returns it cannot publish another editor index.
    pub(crate) fn stop_and_join(&self) {
        self.inner.stop_and_join();
    }
}

impl MonitorInner {
    fn stop_and_join(&self) {
        self.stop.request();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

impl Drop for MonitorInner {
    fn drop(&mut self) {
        self.stop.request();
        let worker = self
            .worker
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct StopSignal {
    stopped: std::sync::atomic::AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}

impl StopSignal {
    fn request(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::Release);
        self.wake.notify_all();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::Acquire)
    }

    fn wait_for_poll(&self, interval: Duration) -> bool {
        if self.is_stopped() {
            return false;
        }
        let guard = self
            .lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.is_stopped() {
            return false;
        }
        let _ = self
            .wake
            .wait_timeout(guard, interval)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        !self.is_stopped()
    }
}

/// Start monitoring followed remotes after the startup synchronization has succeeded.
pub(crate) fn start_federation_type_monitor(
    project_root: &Path,
    config: &FederationConfig,
    lock: Option<&FederationLock>,
    initial: &FederationTypeSyncResult,
    report_error: ReportError,
) -> Result<Option<FederationTypeMonitor>, WakeError> {
    let current = followed_type_revisions(config, initial);
    if current.is_empty() {
        return Ok(None);
    }

    let probe_config = config.clone();
    let probe: Probe = Arc::new(move || probe_followed_federation_type_revisions(&probe_config));
    let sync_config = config.clone();
    let sync_lock = lock.cloned();
    let sync_root = project_root.to_path_buf();
    let synchronize: Synchronize = Arc::new(move || {
        let result =
            sync_federation_types_for_development(&sync_root, &sync_config, sync_lock.as_ref())?;
        Ok(followed_type_revisions(&sync_config, &result))
    });

    start_with_callbacks(
        TYPE_POLL_INTERVAL,
        current,
        probe,
        synchronize,
        report_error,
    )
    .map(Some)
}

fn start_with_callbacks(
    interval: Duration,
    mut current: FederationTypeRevisions,
    probe: Probe,
    synchronize: Synchronize,
    report_error: ReportError,
) -> Result<FederationTypeMonitor, WakeError> {
    let stop = Arc::new(StopSignal::default());
    let worker_stop = Arc::clone(&stop);
    let worker = thread::Builder::new()
        .name("wake-federation-type-sync".to_owned())
        .spawn(move || {
            while worker_stop.wait_for_poll(interval) {
                refresh_once(
                    &worker_stop,
                    &mut current,
                    probe.as_ref(),
                    synchronize.as_ref(),
                    report_error.as_ref(),
                );
            }
        })
        .map_err(|error| {
            WakeError::new(
                "WAKE_IO",
                format!("could not start federation type monitor: {error}"),
            )
        })?;
    Ok(FederationTypeMonitor {
        inner: Arc::new(MonitorInner {
            stop,
            worker: Mutex::new(Some(worker)),
        }),
    })
}

fn refresh_once(
    stop: &StopSignal,
    current: &mut FederationTypeRevisions,
    probe: &(dyn Fn() -> Result<FederationTypeRevisions, WakeError> + Send + Sync),
    synchronize: &(dyn Fn() -> Result<FederationTypeRevisions, WakeError> + Send + Sync),
    report_error: &(dyn Fn(WakeError) + Send + Sync),
) {
    let observed = match probe() {
        Ok(observed) => observed,
        Err(error) => {
            report_error(error);
            return;
        }
    };
    if stop.is_stopped() || observed == *current {
        return;
    }

    match synchronize() {
        Ok(synchronized) => *current = synchronized,
        Err(error) => {
            // Keep `current` unchanged: the same revision remains dirty and is retried on the
            // next poll. The synchronizer itself leaves the stable editor index untouched.
            report_error(error);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use wake_federation_contract::{BuildId, ContainerName, ErrorCode, RemoteConfig};

    use super::*;
    use crate::federation_type_sync::{FederationTypeRevision, SyncedFederationTypes};

    fn revisions(build_id: &str, types_hash: &str) -> FederationTypeRevisions {
        BTreeMap::from([(
            ContainerName::from("catalog"),
            FederationTypeRevision {
                build_id: BuildId::from(build_id),
                types_hash: types_hash.to_owned(),
            },
        )])
    }

    #[test]
    fn unchanged_probe_skips_sync_and_failed_sync_retries_same_revision() {
        let stop = StopSignal::default();
        let mut current = revisions("build-a", "types-a");
        let probes = Mutex::new(VecDeque::from([
            revisions("build-a", "types-a"),
            revisions("build-a", "types-b"),
            revisions("build-a", "types-b"),
        ]));
        let sync_results = Mutex::new(VecDeque::from([
            Err(WakeError::new(ErrorCode::Network.as_str(), "temporary")),
            Ok(revisions("build-a", "types-b")),
        ]));
        let sync_count = AtomicUsize::new(0);
        let errors = AtomicUsize::new(0);

        for _ in 0..3 {
            refresh_once(
                &stop,
                &mut current,
                &|| Ok(probes.lock().unwrap().pop_front().unwrap()),
                &|| {
                    sync_count.fetch_add(1, Ordering::SeqCst);
                    sync_results.lock().unwrap().pop_front().unwrap()
                },
                &|_| {
                    errors.fetch_add(1, Ordering::SeqCst);
                },
            );
        }

        assert_eq!(sync_count.load(Ordering::SeqCst), 2);
        assert_eq!(errors.load(Ordering::SeqCst), 1);
        assert_eq!(current, revisions("build-a", "types-b"));
    }

    #[test]
    fn stop_during_manifest_probe_prevents_a_new_sync_and_joins_worker() {
        let (probe_started_tx, probe_started_rx) = mpsc::channel();
        let (release_probe_tx, release_probe_rx) = mpsc::channel();
        let release_probe_rx = Mutex::new(release_probe_rx);
        let sync_count = Arc::new(AtomicUsize::new(0));
        let sync_count_worker = Arc::clone(&sync_count);
        let monitor = start_with_callbacks(
            Duration::ZERO,
            revisions("build-a", "types-a"),
            Arc::new(move || {
                probe_started_tx.send(()).unwrap();
                release_probe_rx.lock().unwrap().recv().unwrap();
                Ok(revisions("build-b", "types-b"))
            }),
            Arc::new(move || {
                sync_count_worker.fetch_add(1, Ordering::SeqCst);
                Ok(revisions("build-b", "types-b"))
            }),
            Arc::new(|_| {}),
        )
        .unwrap();

        probe_started_rx.recv().unwrap();
        monitor.request_stop();
        release_probe_tx.send(()).unwrap();
        monitor.stop_and_join();

        assert_eq!(sync_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_configuration_with_only_pinned_remotes_starts_no_monitor() {
        let mut config = FederationConfig {
            enabled: true,
            name: ContainerName::from("shell"),
            ..FederationConfig::default()
        };
        config.remotes.insert(
            ContainerName::from("catalog"),
            RemoteConfig {
                manifest_url: "https://catalog.test/wake-federation.json".to_owned(),
                allowed_origins: Vec::new(),
                dev_follow: false,
            },
        );
        let root = tempfile::tempdir().unwrap();
        let initial = FederationTypeSyncResult {
            remotes: vec![SyncedFederationTypes {
                remote: ContainerName::from("catalog"),
                build_id: BuildId::from("build-a"),
                types_hash: "types-a".to_owned(),
                declaration_file: root.path().join("catalog.d.ts"),
            }],
            index_file: root.path().join("index.d.ts"),
        };

        let monitor = start_federation_type_monitor(
            root.path(),
            &config,
            None,
            &initial,
            Arc::new(|_| panic!("no monitor should report")),
        )
        .unwrap();

        assert!(monitor.is_none());
    }
}
