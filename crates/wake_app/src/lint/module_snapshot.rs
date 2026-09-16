//! Per-check sparse overlays and retained resolver filesystem observations.

use crate::{CancellationToken, WakeError};
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use wake_common::{FileSystem, fs::normalize};

pub(super) struct SnapshotFs {
    base: Arc<dyn FileSystem>,
    overlays: BTreeMap<PathBuf, Arc<[u8]>>,
    virtual_dirs: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    cancellation: CancellationToken,
    limits: Limits,
    state: Mutex<State>,
}

#[derive(Clone, Copy)]
struct Limits {
    bytes: usize,
    paths: usize,
}
impl Limits {
    const DEFAULT: Self = Self {
        bytes: 512 * 1024 * 1024,
        paths: 250_000,
    };
}

#[derive(Clone)]
struct Failure {
    kind: io::ErrorKind,
    message: String,
}
impl Failure {
    fn from(error: &io::Error) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
    fn error(&self) -> io::Error {
        io::Error::new(self.kind, self.message.clone())
    }
}
type Retained<T> = Result<T, Failure>;

#[derive(Clone, Copy)]
struct Kind {
    file: bool,
    directory: bool,
    exists: bool,
}

#[derive(Default)]
struct State {
    bytes: usize,
    observed: BTreeSet<PathBuf>,
    kinds: BTreeMap<PathBuf, Kind>,
    contents: BTreeMap<PathBuf, Retained<Arc<[u8]>>>,
    directories: BTreeMap<PathBuf, Retained<Vec<PathBuf>>>,
    canonical: BTreeMap<PathBuf, Retained<PathBuf>>,
    fault: Option<WakeError>,
}

impl State {
    fn observe(&mut self, path: &Path, limits: Limits) -> io::Result<()> {
        if self.observed.contains(path) {
            return Ok(());
        }
        let bytes = path.as_os_str().as_encoded_bytes().len();
        let message = if self.observed.len() >= limits.paths {
            Some("Module filesystem path budget exceeded")
        } else if bytes > limits.bytes.saturating_sub(self.bytes) {
            Some("Module filesystem byte budget exceeded")
        } else {
            None
        };
        if let Some(message) = message {
            self.fault = Some(analysis(message));
            return Err(io::Error::other(message));
        }
        self.bytes += bytes;
        self.observed.insert(path.into());
        Ok(())
    }
}

impl SnapshotFs {
    pub(super) fn new(
        base: Arc<dyn FileSystem>,
        overlays: BTreeMap<PathBuf, Arc<str>>,
        cancellation: CancellationToken,
    ) -> Result<Self, WakeError> {
        Self::with_limits(base, overlays, cancellation, Limits::DEFAULT)
    }

    fn with_limits(
        base: Arc<dyn FileSystem>,
        overlays: BTreeMap<PathBuf, Arc<str>>,
        cancellation: CancellationToken,
        limits: Limits,
    ) -> Result<Self, WakeError> {
        cancellation.check()?;
        let mut state = State::default();
        let mut files = BTreeMap::new();
        let mut virtual_dirs: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();
        for (path, text) in overlays {
            cancellation.check()?;
            let path = normalize(&path);
            if !path.is_absolute() {
                return Err(analysis("Module overlay paths must be absolute"));
            }
            state
                .observe(&path, limits)
                .map_err(|_| state.fault.clone().expect("latched budget error"))?;
            if text.len() > limits.bytes.saturating_sub(state.bytes) {
                return Err(analysis("Module filesystem byte budget exceeded"));
            }
            state.bytes += text.len();
            files.insert(path.clone(), Arc::from(text.as_bytes()));
            let mut child = path.as_path();
            while let Some(parent) = child.parent() {
                if !virtual_dirs.contains_key(parent) && base.is_file(parent) {
                    return Err(analysis(
                        "Module virtual directory conflicts with a physical file",
                    ));
                }
                state
                    .observe(parent, limits)
                    .map_err(|_| state.fault.clone().expect("latched budget error"))?;
                virtual_dirs
                    .entry(parent.into())
                    .or_default()
                    .insert(child.into());
                child = parent;
            }
        }
        if files.keys().any(|file| virtual_dirs.contains_key(file)) {
            return Err(analysis(
                "Module overlay file conflicts with a virtual directory",
            ));
        }
        if files.len().saturating_add(virtual_dirs.len()) > limits.paths {
            return Err(analysis("Module filesystem path budget exceeded"));
        }
        Ok(Self {
            base,
            overlays: files,
            virtual_dirs,
            cancellation,
            limits,
            state: Mutex::new(state),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn check(&self) -> Result<(), WakeError> {
        self.cancellation.check()?;
        self.state().fault.clone().map_or(Ok(()), Err)
    }

    pub(super) fn observed_paths(&self) -> Vec<PathBuf> {
        self.state().observed.iter().cloned().collect()
    }

    fn access<T>(
        &self,
        path: &Path,
        operation: impl FnOnce(&mut State, &Path) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut state = self.state();
        if self.cancellation.is_cancelled() {
            state.fault = Some(WakeError::cancelled());
        }
        if let Some(error) = &state.fault {
            return Err(io::Error::other(error.message.clone()));
        }
        let path = normalize(path);
        state.observe(&path, self.limits)?;
        let result = operation(&mut state, &path);
        if self.cancellation.is_cancelled() {
            state.fault = Some(WakeError::cancelled());
            return Err(io::Error::other("Module check was cancelled"));
        }
        if let Err(error) = &result
            && state.fault.is_none()
            && !matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::NotADirectory
                    | io::ErrorKind::IsADirectory
            )
        {
            state.fault = Some(WakeError::new("WAKE_LINT_IO", error.to_string()).at(&path));
        }
        result
    }

    fn kind(&self, state: &mut State, path: &Path) -> Kind {
        *state.kinds.entry(path.into()).or_insert_with(|| {
            if self.overlays.contains_key(path) {
                return Kind {
                    file: true,
                    directory: false,
                    exists: true,
                };
            }
            if self.virtual_dirs.contains_key(path) {
                return Kind {
                    file: false,
                    directory: true,
                    exists: true,
                };
            }
            let file = self.base.is_file(path);
            let directory = !file && self.base.is_dir(path);
            Kind {
                file,
                directory,
                exists: file || directory || self.base.exists(path),
            }
        })
    }
}

fn analysis(message: &str) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", message)
}
fn missing(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} is absent from the module snapshot", path.display()),
    )
}
fn restore<T: Clone>(value: &Retained<T>) -> io::Result<T> {
    match value {
        Ok(value) => Ok(value.clone()),
        Err(error) => Err(error.error()),
    }
}

impl FileSystem for SnapshotFs {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.access(path, |state, path| {
            if let Some(retained) = state.canonical.get(path) {
                return restore(retained);
            }
            let result = if !self.kind(state, path).exists {
                Err(missing(path))
            } else if self.overlays.contains_key(path) || self.virtual_dirs.contains_key(path) {
                Ok(path.into())
            } else {
                self.base.canonicalize(path).map(|path| normalize(&path))
            };
            if let Ok(canonical) = &result {
                state.observe(canonical, self.limits)?;
            }
            state.canonical.insert(
                path.into(),
                result.as_ref().map(Clone::clone).map_err(Failure::from),
            );
            result
        })
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes).map_err(|error| {
            let error = io::Error::new(io::ErrorKind::InvalidData, error);
            self.state().fault = Some(WakeError::new("WAKE_LINT_IO", error.to_string()).at(path));
            error
        })
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.access(path, |state, path| {
            if let Some(bytes) = self.overlays.get(path) {
                return Ok(bytes.to_vec());
            }
            if let Some(retained) = state.contents.get(path) {
                return restore(retained).map(|bytes| bytes.to_vec());
            }
            let result = if self.kind(state, path).file {
                self.base.read(path)
            } else {
                Err(missing(path))
            };
            let result = match result {
                Ok(bytes) => {
                    if bytes.len() > self.limits.bytes.saturating_sub(state.bytes) {
                        state.fault = Some(analysis("Module filesystem byte budget exceeded"));
                        return Err(io::Error::other("Module filesystem byte budget exceeded"));
                    }
                    state.bytes += bytes.len();
                    Ok(Arc::<[u8]>::from(bytes))
                }
                Err(error) => Err(Failure::from(&error)),
            };
            let output = restore(&result).map(|bytes| bytes.to_vec());
            state.contents.insert(path.into(), result);
            output
        })
    }

    fn exists(&self, path: &Path) -> bool {
        self.access(path, |state, path| Ok(self.kind(state, path).exists))
            .unwrap_or(false)
    }
    fn is_file(&self, path: &Path) -> bool {
        self.access(path, |state, path| Ok(self.kind(state, path).file))
            .unwrap_or(false)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.access(path, |state, path| Ok(self.kind(state, path).directory))
            .unwrap_or(false)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.access(path, |state, path| {
            if let Some(retained) = state.directories.get(path) {
                return restore(retained);
            }
            let result: io::Result<Vec<PathBuf>> = if !self.kind(state, path).directory {
                Err(missing(path))
            } else {
                let disk = match self.base.read_dir(path) {
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            && self.virtual_dirs.contains_key(path) =>
                    {
                        Ok(Vec::new())
                    }
                    result => result,
                };
                disk.map(|paths| {
                    let mut paths: BTreeSet<_> = paths.iter().map(|path| normalize(path)).collect();
                    if let Some(virtual_children) = self.virtual_dirs.get(path) {
                        paths.extend(virtual_children.iter().cloned());
                    }
                    paths.into_iter().collect()
                })
            };
            if let Ok(children) = &result {
                for child in children {
                    state.observe(child, self.limits)?;
                }
            }
            state.directories.insert(
                path.into(),
                result.as_ref().map(Clone::clone).map_err(Failure::from),
            );
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::MemoryFileSystem;

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:/lint-project"
        } else {
            "/lint-project"
        })
    }

    #[test]
    fn sparse_overlay_preserves_disk_siblings_and_creates_virtual_directories() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([
            (root.join("a.ts"), "disk"),
            (root.join("sibling.ts"), "sibling"),
        ]));
        let snapshot = SnapshotFs::new(
            disk,
            [
                (root.join("a.ts"), Arc::from("unsaved")),
                (root.join("new/deep/b.ts"), Arc::from("virtual")),
            ]
            .into(),
            CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(
            snapshot.read_to_string(&root.join("a.ts")).unwrap(),
            "unsaved"
        );
        assert_eq!(
            snapshot.read_to_string(&root.join("sibling.ts")).unwrap(),
            "sibling"
        );
        assert!(snapshot.is_dir(&root.join("new/deep")));
        assert!(
            snapshot
                .read_dir(&root)
                .unwrap()
                .contains(&root.join("new"))
        );
        assert_eq!(
            snapshot.read_dir(&root.join("new")).unwrap(),
            [root.join("new/deep")]
        );
        assert_eq!(
            snapshot.canonicalize(&root.join("new/deep/b.ts")).unwrap(),
            root.join("new/deep/b.ts")
        );
        assert_eq!(
            snapshot
                .read_to_string(&root.join("new/deep/b.ts"))
                .unwrap(),
            "virtual"
        );
        snapshot.check().unwrap();
    }

    #[test]
    fn source_bytes_negative_reads_and_directory_observations_are_retained_for_the_generation() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([(root.join("a.ts"), "first")]));
        let snapshot =
            SnapshotFs::new(disk.clone(), BTreeMap::new(), CancellationToken::default()).unwrap();
        assert_eq!(
            snapshot.read_to_string(&root.join("a.ts")).unwrap(),
            "first"
        );
        assert!(!snapshot.is_file(&root.join("later.ts")));
        assert!(snapshot.read(&root.join("missing.ts")).is_err());
        assert_eq!(snapshot.read_dir(&root).unwrap(), [root.join("a.ts")]);
        disk.insert(root.join("a.ts"), "changed");
        disk.insert(root.join("later.ts"), "new");
        disk.insert(root.join("missing.ts"), "new");
        assert_eq!(
            snapshot.read_to_string(&root.join("a.ts")).unwrap(),
            "first"
        );
        assert!(snapshot.read(&root.join("later.ts")).is_err());
        assert!(!snapshot.exists(&root.join("missing.ts")));
        assert_eq!(snapshot.read_dir(&root).unwrap(), [root.join("a.ts")]);
        assert!(snapshot.observed_paths().contains(&root.join("later.ts")));
        let next = SnapshotFs::new(disk, BTreeMap::new(), CancellationToken::default()).unwrap();
        assert_eq!(next.read_to_string(&root.join("a.ts")).unwrap(), "changed");
        assert!(next.is_file(&root.join("later.ts")));
    }

    #[test]
    fn budgets_and_cancellation_cannot_be_swallowed_by_boolean_filesystem_queries() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([(
            root.join("a.ts"),
            "source exceeds byte budget",
        )]));
        let limits = Limits {
            bytes: 2,
            paths: 10,
        };
        let snapshot = SnapshotFs::with_limits(
            disk.clone(),
            BTreeMap::new(),
            CancellationToken::default(),
            limits,
        )
        .unwrap();
        assert!(snapshot.read(&root.join("a.ts")).is_err());
        assert_eq!(snapshot.check().unwrap_err().code, "WAKE_LINT_ANALYSIS");
        assert!(!snapshot.is_file(&root.join("a.ts")));
        let snapshot = SnapshotFs::with_limits(
            disk.clone(),
            BTreeMap::new(),
            CancellationToken::default(),
            Limits {
                bytes: 1024,
                paths: 1,
            },
        )
        .unwrap();
        assert!(snapshot.is_file(&root.join("a.ts")));
        assert!(!snapshot.exists(&root.join("next.ts")));
        assert_eq!(snapshot.check().unwrap_err().code, "WAKE_LINT_ANALYSIS");
        let cancellation = CancellationToken::default();
        let snapshot = SnapshotFs::new(disk, BTreeMap::new(), cancellation.clone()).unwrap();
        cancellation.cancel();
        assert!(!snapshot.is_file(&root.join("a.ts")));
        assert_eq!(snapshot.check().unwrap_err().code, "WAKE_CANCELLED");
    }

    #[test]
    fn directory_entries_and_virtual_parents_obey_the_same_snapshot_limits() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([
            (root.join("a.ts"), "a"),
            (root.join("b.ts"), "b"),
        ]));
        let snapshot = SnapshotFs::with_limits(
            disk.clone(),
            BTreeMap::new(),
            CancellationToken::default(),
            Limits {
                bytes: 1024,
                paths: 2,
            },
        )
        .unwrap();
        assert!(snapshot.read_dir(&root).is_err());
        assert_eq!(snapshot.check().unwrap_err().code, "WAKE_LINT_ANALYSIS");
        assert!(
            SnapshotFs::new(
                disk,
                [(root.join("a.ts/child.ts"), Arc::from("child"))].into(),
                CancellationToken::default()
            )
            .is_err()
        );
    }
}
