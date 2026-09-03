//! Generation-scoped read-through filesystem views.
//!
//! A build generation freezes each observable query at its first completed result. Retained and
//! one-shot views share those results, including failures. Advancing the owner keeps one stable
//! filesystem proxy while replacing its observation epoch.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use wake_common::{FileSystem, FxHashMap};

use crate::BuildOutput;
use crate::session::{BuildOptions, BuildRequest, BuildSession};

/// The sole owner of every compilation view in one product build generation.
///
/// A retained application session and transient container/provider views created by this owner
/// receive the same stable filesystem proxy. [`BuildGeneration::advance_generation`] replaces the
/// proxy's observation epoch before the retained session is invalidated for the next watcher batch.
/// The owner must not advance while a build is executing.
pub struct BuildGeneration {
    fs: GenerationFileSystem,
}

impl BuildGeneration {
    pub fn new(source: Arc<dyn FileSystem>) -> Self {
        Self {
            fs: GenerationFileSystem::new(source),
        }
    }

    /// Create the retained application view owned by a build context or watcher.
    pub fn retained_session(&self, options: BuildOptions) -> BuildSession {
        BuildSession::new(self.fs.view(), options)
    }

    /// Share the current observation proxy with product-owned planning that must precede a view.
    pub fn file_system_view(&self) -> Arc<dyn FileSystem> {
        self.fs.view()
    }

    /// Compile one transient view against the current generation observations.
    pub fn build_once(&mut self, options: BuildOptions, request: BuildRequest) -> BuildOutput {
        BuildSession::new_one_shot(self.fs.view(), options).build_once(request)
    }

    /// Accept the next watcher batch and discard every observation from the previous generation.
    pub fn advance_generation(&mut self) -> u64 {
        self.fs.advance_generation()
    }

    pub fn generation(&self) -> u64 {
        self.fs.generation()
    }
}

/// A read-only filesystem capability whose observed facts are stable for one build generation.
///
/// This is a lazy, query-scoped snapshot because [`FileSystem`] has no transaction primitive. The
/// first completed result of each [`FileSystem`] method for an exact path spelling is retained and
/// replayed to every clone. Only [`BuildGeneration`] may replace the active observation epoch.
///
/// Path keys preserve the exact platform [`OsString`] representation. They are never normalized,
/// canonicalized, resolved through symlinks/reparse points, or case-folded. This avoids collapsing
/// `symlink/../x`, trailing-separator queries, or distinct entries in a case-sensitive Windows
/// directory. Different spellings that happen to reach the same object intentionally remain
/// different queries.
///
/// The seven trait methods are separate observation families. This type therefore guarantees
/// repeatability within each method, not a cross-method atomic filesystem transaction. For example,
/// an `exists(false)` observation does not prevent a later, first-time `read` from observing bytes
/// added in the meantime.
///
/// I/O failures preserve [`io::ErrorKind`] and either the raw OS error code or, for custom errors,
/// the captured display message. Arbitrary error sources and downcast payloads are not cloneable
/// through `io::Error` and are intentionally outside the replay contract.
#[derive(Clone)]
pub(crate) struct GenerationFileSystem {
    shared: Arc<GenerationShared>,
}

struct GenerationShared {
    source: Arc<dyn FileSystem>,
    generation: AtomicU64,
    active: RwLock<Arc<GenerationState>>,
}

#[derive(Default)]
struct GenerationState {
    canonical_paths: QueryCache<CachedIo<PathBuf>>,
    text_contents: QueryCache<CachedIo<String>>,
    contents: QueryCache<CachedIo<Vec<u8>>>,
    existence: QueryCache<bool>,
    files: QueryCache<bool>,
    directories: QueryCache<bool>,
    directory_entries: QueryCache<CachedIo<Vec<PathBuf>>>,
}

impl GenerationFileSystem {
    /// Start a generation backed by `source` with no observed filesystem facts.
    pub fn new(source: Arc<dyn FileSystem>) -> Self {
        Self {
            shared: Arc::new(GenerationShared {
                source,
                generation: AtomicU64::new(0),
                active: RwLock::new(Arc::new(GenerationState::default())),
            }),
        }
    }

    /// Return a trait-object view sharing this generation's observation cache.
    pub fn view(&self) -> Arc<dyn FileSystem> {
        Arc::new(self.clone())
    }

    fn active(&self) -> Arc<GenerationState> {
        self.shared
            .active
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn advance_generation(&self) -> u64 {
        *self
            .shared
            .active
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            Arc::new(GenerationState::default());
        self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }
}

impl FileSystem for GenerationFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let state = self.active();
        let cell = state.canonical_paths.cell(path);
        cell.get_or_init(|| CachedIo::capture(self.shared.source.canonicalize(path)))
            .replay()
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let state = self.active();
        let cell = state.text_contents.cell(path);
        cell.get_or_init(|| CachedIo::capture(self.shared.source.read_to_string(path)))
            .replay()
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let state = self.active();
        let cell = state.contents.cell(path);
        cell.get_or_init(|| CachedIo::capture(self.shared.source.read(path)))
            .replay()
    }

    fn exists(&self, path: &Path) -> bool {
        let state = self.active();
        let cell = state.existence.cell(path);
        *cell.get_or_init(|| self.shared.source.exists(path))
    }

    fn is_file(&self, path: &Path) -> bool {
        let state = self.active();
        let cell = state.files.cell(path);
        *cell.get_or_init(|| self.shared.source.is_file(path))
    }

    fn is_dir(&self, path: &Path) -> bool {
        let state = self.active();
        let cell = state.directories.cell(path);
        *cell.get_or_init(|| self.shared.source.is_dir(path))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let state = self.active();
        let cell = state.directory_entries.cell(path);
        cell.get_or_init(|| CachedIo::capture(self.shared.source.read_dir(path)))
            .replay()
    }
}

struct QueryCache<T> {
    entries: Mutex<FxHashMap<OsString, Arc<OnceLock<T>>>>,
}

impl<T> Default for QueryCache<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(FxHashMap::default()),
        }
    }
}

impl<T> QueryCache<T> {
    fn cell(&self, path: &Path) -> Arc<OnceLock<T>> {
        let key = path.as_os_str().to_os_string();
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        entries
            .entry(key)
            .or_insert_with(|| Arc::new(OnceLock::new()))
            .clone()
    }
}

#[derive(Clone)]
enum CachedIo<T> {
    Ok(T),
    Err(IoErrorSnapshot),
}

impl<T> CachedIo<T> {
    fn capture(result: io::Result<T>) -> Self {
        match result {
            Ok(value) => Self::Ok(value),
            Err(error) => Self::Err(IoErrorSnapshot::capture(error)),
        }
    }
}

impl<T: Clone> CachedIo<T> {
    fn replay(&self) -> io::Result<T> {
        match self {
            Self::Ok(value) => Ok(value.clone()),
            Self::Err(error) => Err(error.replay()),
        }
    }
}

#[derive(Clone)]
struct IoErrorSnapshot {
    kind: io::ErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl IoErrorSnapshot {
    fn capture(error: io::Error) -> Self {
        Self {
            kind: error.kind(),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }

    fn replay(&self) -> io::Error {
        self.raw_os_error.map_or_else(
            || io::Error::new(self.kind, self.message.clone()),
            io::Error::from_raw_os_error,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    use tempfile::tempdir;
    use wake_common::{MemoryFileSystem, OsFileSystem};

    use super::*;

    fn error_signature(error: io::Error) -> (io::ErrorKind, Option<i32>, String) {
        (error.kind(), error.raw_os_error(), error.to_string())
    }

    #[test]
    fn same_generation_replays_content_metadata_directory_and_errors_across_views() {
        let directory = tempdir().unwrap();
        let src = directory.path().join("src");
        let index = src.join("index.js");
        let original = src.join("original.js");
        let later = src.join("later.js");
        let created = directory.path().join("created");
        fs::create_dir(&src).unwrap();
        fs::write(&index, "old").unwrap();
        fs::write(&original, "original").unwrap();

        let generation = GenerationFileSystem::new(Arc::new(OsFileSystem));
        let first = generation.view();
        let second = generation.view();

        assert_eq!(first.read_to_string(&index).unwrap(), "old");
        assert_eq!(first.read(&index).unwrap(), b"old");
        assert!(first.exists(&index));
        assert!(first.is_file(&index));
        assert!(!first.is_dir(&index));
        let original_listing = first.read_dir(&src).unwrap();
        assert_eq!(original_listing.len(), 2);
        assert!(!first.exists(&later));
        assert!(!first.is_file(&later));
        assert!(!first.is_dir(&later));
        assert!(!first.is_dir(&created));
        let missing_read = error_signature(first.read(&later).unwrap_err());
        let missing_text = error_signature(first.read_to_string(&later).unwrap_err());
        let missing_directory = error_signature(first.read_dir(&created).unwrap_err());
        assert_eq!(missing_read.0, io::ErrorKind::NotFound);
        assert_eq!(missing_text.0, io::ErrorKind::NotFound);
        assert_eq!(missing_directory.0, io::ErrorKind::NotFound);

        fs::write(&index, "new").unwrap();
        fs::write(&later, "later").unwrap();
        fs::create_dir(&created).unwrap();
        let created_entry = created.join("entry.js");
        fs::write(&created_entry, "created").unwrap();

        assert_eq!(second.read(&index).unwrap(), b"old");
        assert_eq!(second.read_to_string(&index).unwrap(), "old");
        assert!(!second.exists(&later));
        assert!(!second.is_file(&later));
        assert!(!second.is_dir(&later));
        assert!(!second.is_dir(&created));
        assert_eq!(
            error_signature(second.read(&later).unwrap_err()),
            missing_read
        );
        assert_eq!(
            error_signature(second.read_to_string(&later).unwrap_err()),
            missing_text
        );
        assert_eq!(second.read_dir(&src).unwrap(), original_listing);
        assert_eq!(
            error_signature(second.read_dir(&created).unwrap_err()),
            missing_directory
        );

        assert_eq!(generation.advance_generation(), 1);
        assert_eq!(second.read_to_string(&index).unwrap(), "new");
        assert!(second.exists(&later));
        assert!(second.is_file(&later));
        assert!(second.is_dir(&created));
        assert_eq!(second.read_to_string(&later).unwrap(), "later");
        let mut next_listing = second.read_dir(&src).unwrap();
        next_listing.sort();
        let mut expected_listing = vec![index, later, original];
        expected_listing.sort();
        assert_eq!(next_listing, expected_listing);
        assert_eq!(second.read_dir(&created).unwrap(), vec![created_entry]);
    }

    #[test]
    fn custom_text_errors_replay_kind_and_message_until_the_next_generation() {
        let source = Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            vec![0xff_u8],
        )]));
        let generation = GenerationFileSystem::new(source.clone());
        let first_error = error_signature(
            generation
                .read_to_string(Path::new("src/index.js"))
                .unwrap_err(),
        );
        assert_eq!(first_error.0, io::ErrorKind::InvalidData);
        assert_eq!(first_error.1, None);

        source.insert("src/index.js", "valid");

        assert_eq!(
            error_signature(
                generation
                    .view()
                    .read_to_string(Path::new("src/index.js"))
                    .unwrap_err()
            ),
            first_error
        );
        assert_eq!(generation.advance_generation(), 1);
        assert_eq!(
            generation
                .read_to_string(Path::new("src/index.js"))
                .unwrap(),
            "valid"
        );
    }

    #[test]
    fn path_keys_preserve_case_for_case_sensitive_windows_directories() {
        let source = Arc::new(MemoryFileSystem::from_files([
            ("src/Entry.js", "upper-old"),
            ("src/entry.js", "lower-old"),
        ]));
        let generation = GenerationFileSystem::new(source.clone());

        assert_eq!(
            generation
                .read_to_string(Path::new("src/Entry.js"))
                .unwrap(),
            "upper-old"
        );
        assert_eq!(
            generation
                .read_to_string(Path::new("src/entry.js"))
                .unwrap(),
            "lower-old"
        );

        source.insert("src/Entry.js", "upper-new");
        source.insert("src/entry.js", "lower-new");

        assert_eq!(
            generation
                .read_to_string(Path::new("src/Entry.js"))
                .unwrap(),
            "upper-old"
        );
        assert_eq!(
            generation
                .read_to_string(Path::new("src/entry.js"))
                .unwrap(),
            "lower-old"
        );
        assert_eq!(generation.advance_generation(), 1);
        assert_eq!(
            generation
                .read_to_string(Path::new("src/Entry.js"))
                .unwrap(),
            "upper-new"
        );
        assert_eq!(
            generation
                .read_to_string(Path::new("src/entry.js"))
                .unwrap(),
            "lower-new"
        );
    }

    #[test]
    fn one_shot_views_share_one_epoch_and_the_next_generation_reads_new_bytes() {
        let source = Arc::new(MemoryFileSystem::from_files([
            (
                "src/first.js",
                "import { value } from './shared.js'; globalThis.first = value;",
            ),
            (
                "src/second.js",
                "import { value } from './shared.js'; globalThis.second = value;",
            ),
            ("src/shared.js", "export const value = 'generation-old';"),
        ]));
        let mut generation = BuildGeneration::new(source.clone());

        let first =
            generation.build_once(BuildOptions::default(), BuildRequest::new("src/first.js"));
        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        assert!(first.bundle.contains("generation-old"));

        source.insert("src/shared.js", "export const value = 'generation-new';");
        let second =
            generation.build_once(BuildOptions::default(), BuildRequest::new("src/second.js"));
        assert!(!second.has_errors(), "{:?}", second.diagnostics);
        assert!(second.bundle.contains("generation-old"));
        assert!(!second.bundle.contains("generation-new"));

        assert_eq!(generation.advance_generation(), 1);
        let next =
            generation.build_once(BuildOptions::default(), BuildRequest::new("src/second.js"));
        assert!(!next.has_errors(), "{:?}", next.diagnostics);
        assert!(next.bundle.contains("generation-new"));
    }

    #[test]
    fn retained_session_follows_the_owner_epoch_after_one_explicit_advance() {
        let source = Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            "globalThis.generation = 'retained-old';",
        )]));
        let mut generation = BuildGeneration::new(source.clone());
        let mut session = generation.retained_session(BuildOptions::default());
        let request = BuildRequest::new("src/index.js");

        let first = session.build_current(request.clone());
        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        assert!(first.bundle.contains("retained-old"));

        source.insert("src/index.js", "globalThis.generation = 'retained-new';");
        assert_eq!(generation.advance_generation(), 1);
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let next = session.build_current(request);
        assert!(!next.has_errors(), "{:?}", next.diagnostics);
        assert!(next.bundle.contains("retained-new"));
    }

    struct CountingFileSystem {
        inner: MemoryFileSystem,
        read_count: AtomicUsize,
    }

    impl FileSystem for CountingFileSystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            self.inner.canonicalize(path)
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            self.inner.read_to_string(path)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.read_count.fetch_add(1, Ordering::Relaxed);
            thread::sleep(Duration::from_millis(10));
            self.inner.read(path)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            self.inner.is_file(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.inner.read_dir(path)
        }
    }

    #[test]
    fn concurrent_views_single_flight_the_first_query() {
        const READERS: usize = 8;
        let source = Arc::new(CountingFileSystem {
            inner: MemoryFileSystem::from_files([("src/index.js", "stable")]),
            read_count: AtomicUsize::new(0),
        });
        let generation = GenerationFileSystem::new(source.clone());
        let barrier = Arc::new(Barrier::new(READERS));
        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let view = generation.view();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    String::from_utf8(view.read(Path::new("src/index.js")).unwrap()).unwrap()
                })
            })
            .collect();

        for reader in readers {
            assert_eq!(reader.join().unwrap(), "stable");
        }
        assert_eq!(source.read_count.load(Ordering::Relaxed), 1);
    }

    struct MutableCanonicalFileSystem {
        identity: Mutex<PathBuf>,
    }

    impl FileSystem for MutableCanonicalFileSystem {
        fn canonicalize(&self, _path: &Path) -> io::Result<PathBuf> {
            Ok(self.identity.lock().unwrap().clone())
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
        }

        fn exists(&self, _path: &Path) -> bool {
            false
        }

        fn is_file(&self, _path: &Path) -> bool {
            false
        }

        fn is_dir(&self, _path: &Path) -> bool {
            false
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ))
        }
    }

    #[test]
    fn canonical_identity_is_replayed_until_generation_advance() {
        let source = Arc::new(MutableCanonicalFileSystem {
            identity: Mutex::new(PathBuf::from("identity-old")),
        });
        let generation = GenerationFileSystem::new(source.clone());
        let first = generation.view();
        let second = generation.view();
        let request = Path::new("logical/input.ts");

        assert_eq!(
            first.canonicalize(request).unwrap(),
            Path::new("identity-old")
        );
        *source.identity.lock().unwrap() = PathBuf::from("identity-new");
        assert_eq!(
            second.canonicalize(request).unwrap(),
            Path::new("identity-old")
        );

        assert_eq!(generation.advance_generation(), 1);
        assert_eq!(
            first.canonicalize(request).unwrap(),
            Path::new("identity-new")
        );
    }
}
