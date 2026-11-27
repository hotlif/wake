//! Filesystem observations used to revalidate a completed Worker build.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wake_common::{FileSystem, FxHashMap};
use xxhash_rust::xxh3::xxh3_128;

use crate::generation::IoErrorSnapshot;

type Queries<T> = FxHashMap<OsString, T>;
type ObservedIo<T> = Result<T, IoErrorSnapshot>;

#[derive(Eq, PartialEq)]
struct ContentFingerprint {
    len: usize,
    hash: u128,
}

fn content<T: AsRef<[u8]>>(result: &io::Result<T>) -> ObservedIo<ContentFingerprint> {
    result
        .as_ref()
        .map_err(IoErrorSnapshot::capture)
        .map(|value| {
            let bytes = value.as_ref();
            ContentFingerprint {
                len: bytes.len(),
                hash: xxh3_128(bytes),
            }
        })
}

#[derive(Default)]
pub(super) struct WorkerInputs {
    canonical_paths: Queries<ObservedIo<OsString>>,
    text: Queries<ObservedIo<ContentFingerprint>>,
    bytes: Queries<ObservedIo<ContentFingerprint>>,
    existence: Queries<bool>,
    files: Queries<bool>,
    directories: Queries<bool>,
    listings: Queries<ObservedIo<Box<[OsString]>>>,
}

impl WorkerInputs {
    pub(super) fn matches(&self, fs: &dyn FileSystem) -> bool {
        fn all_match<T: Eq>(queries: &Queries<T>, current: impl Fn(&Path) -> T) -> bool {
            queries
                .iter()
                .all(|(path, recorded)| current(Path::new(path)) == *recorded)
        }

        all_match(&self.existence, |p| fs.exists(p))
            && all_match(&self.files, |p| fs.is_file(p))
            && all_match(&self.directories, |p| fs.is_dir(p))
            && all_match(&self.canonical_paths, |p| {
                fs.canonicalize(p)
                    .map(PathBuf::into_os_string)
                    .map_err(|err| IoErrorSnapshot::capture(&err))
            })
            && all_match(&self.listings, |p| {
                fs.read_dir(p)
                    .map(|paths| paths.into_iter().map(PathBuf::into_os_string).collect())
                    .map_err(|err| IoErrorSnapshot::capture(&err))
            })
            && all_match(&self.text, |p| content(&fs.read_to_string(p)))
            && all_match(&self.bytes, |p| content(&fs.read(p)))
    }
}

pub(super) struct WorkerInputRecorder {
    source: Arc<dyn FileSystem>,
    inputs: Mutex<Option<WorkerInputs>>,
}

impl WorkerInputRecorder {
    pub(super) fn new(source: Arc<dyn FileSystem>) -> Self {
        Self {
            source,
            inputs: Mutex::new(Some(WorkerInputs::default())),
        }
    }

    pub(super) fn finish(self) -> Option<WorkerInputs> {
        self.inputs.into_inner().ok().flatten()
    }

    fn record<T: Eq>(
        &self,
        path: &Path,
        value: T,
        family: fn(&mut WorkerInputs) -> &mut Queries<T>,
    ) {
        let Ok(mut state) = self.inputs.lock() else {
            return;
        };
        let Some(inputs) = state.as_mut() else { return };
        let queries = family(inputs);
        let key = path.as_os_str();
        match queries.get(key) {
            Some(previous) if previous != &value => *state = None,
            Some(_) => {}
            None => {
                queries.insert(key.to_owned(), value);
            }
        }
    }
}

impl FileSystem for WorkerInputRecorder {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let result = self.source.canonicalize(path);
        let spelling = result
            .as_ref()
            .map(|path| path.as_os_str().to_owned())
            .map_err(IoErrorSnapshot::capture);
        self.record(path, spelling, |inputs| &mut inputs.canonical_paths);
        result
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let result = self.source.read_to_string(path);
        self.record(path, content(&result), |inputs| &mut inputs.text);
        result
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let result = self.source.read(path);
        self.record(path, content(&result), |inputs| &mut inputs.bytes);
        result
    }

    fn exists(&self, path: &Path) -> bool {
        let exists = self.source.exists(path);
        self.record(path, exists, |inputs| &mut inputs.existence);
        exists
    }

    fn is_file(&self, path: &Path) -> bool {
        let is_file = self.source.is_file(path);
        self.record(path, is_file, |inputs| &mut inputs.files);
        is_file
    }

    fn is_dir(&self, path: &Path) -> bool {
        let is_dir = self.source.is_dir(path);
        self.record(path, is_dir, |inputs| &mut inputs.directories);
        is_dir
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let result = self.source.read_dir(path);
        let entries = result
            .as_ref()
            .map(|paths| {
                paths
                    .iter()
                    .map(|path| path.as_os_str().to_owned())
                    .collect()
            })
            .map_err(IoErrorSnapshot::capture);
        self.record(path, entries, |inputs| &mut inputs.listings);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::MemoryFileSystem;

    #[test]
    fn distinct_query_spellings_replay_their_own_errors() {
        let fs = Arc::new(MemoryFileSystem::new());
        let recorder = WorkerInputRecorder::new(fs.clone());
        let plain = recorder.read(Path::new("src/missing.js")).unwrap_err();
        let dotted = recorder.read(Path::new("src/./missing.js")).unwrap_err();
        assert_ne!(plain.to_string(), dotted.to_string());
        assert!(recorder.finish().unwrap().matches(fs.as_ref()));
    }

    #[test]
    fn contradictory_reads_during_a_build_disable_reuse() {
        let path = Path::new("input.js");
        let fs = Arc::new(MemoryFileSystem::from_files([(path, "first")]));
        let recorder = WorkerInputRecorder::new(fs.clone());
        assert_eq!(recorder.read_to_string(path).unwrap(), "first");
        fs.insert(path, "other");
        assert_eq!(recorder.read_to_string(path).unwrap(), "other");
        assert!(recorder.finish().is_none());
    }
}
