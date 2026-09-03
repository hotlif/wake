//! # PnP 感知文件系统
//!
//! [`PnpFileSystem`] 包裹任意内层 [`FileSystem`]，在 I/O 边界透明处理两件 PnP 专属的事：
//!
//! 1. **虚拟路径**：先经 [`resolve_virtual`] 把 `.yarn/__virtual__/…` 映射回真实物理路径；
//! 2. **zip 归档**：物理路径若含 `*.zip/` 段，则把该段之前当作归档文件、之后当作内部条目，
//!    经 [`ZipArchive`](wake_common::zip::ZipArchive) 读取 stored/DEFLATE 字节。
//!
//! 这是整个 PnP 支持的**核心接缝**——一旦文件访问 zip 感知，[`crate::Resolver`] 的
//! `is_file`/`is_dir` 探测、bundler 的源码读取、codegen 全都无需改动即可命中 zip 内文件
//! （DESIGN §3.3：编译核心只认 `FileSystem` trait）。非 PnP 项目根本不构造它，零回归。

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use wake_common::zip::ZipArchive;
use wake_common::{FileSystem, FxHashMap, fs::normalize};

use crate::pnp::resolve_virtual;

/// 包裹内层 FS，叠加虚拟路径解析 + zip 归档读取。
pub struct PnpFileSystem {
    inner: Arc<dyn FileSystem>,
    /// 已解析归档与失效 revision。revision 使失效能胜过并发中的首次打开。
    archive_cache: Mutex<ArchiveCache>,
}

#[derive(Default)]
struct ArchiveCache {
    full_revision: u64,
    revisions: FxHashMap<PathBuf, u64>,
    /// 归档路径（归一化）→ 已解析归档。首次访问某 zip 时读盘+解目录，之后复用。
    entries: FxHashMap<PathBuf, Arc<ZipArchive>>,
}

impl PnpFileSystem {
    pub fn new(inner: Arc<dyn FileSystem>) -> PnpFileSystem {
        PnpFileSystem {
            inner,
            archive_cache: Mutex::new(ArchiveCache::default()),
        }
    }

    /// 逻辑路径 → 物理路径（剥离虚拟段）。
    fn physical(&self, path: &Path) -> PathBuf {
        resolve_virtual(&normalize(path))
    }

    /// Project one logical PnP path onto the physical path used for I/O.
    ///
    /// The logical path remains the module identity; callers use this projection only at physical
    /// boundaries such as diagnostics and file watching.
    pub fn physical_path(&self, path: &Path) -> PathBuf {
        self.physical(path)
    }

    /// Return the physical filesystem object whose mutation can change `path`.
    ///
    /// Entries inside a Yarn cache archive are watched through the archive file itself. Virtual
    /// non-archive paths are watched through their `resolveVirtual` target.
    pub fn watch_path(&self, path: &Path) -> PathBuf {
        let physical = self.physical(path);
        split_zip(&physical).map_or(physical, |(archive, _)| archive)
    }

    /// 清除 archive 级缓存；PnP 清单或 lock 变化后下一 generation 重新打开归档。
    pub fn clear_cache(&self) {
        let mut cache = self.archive_cache.lock().unwrap();
        cache.full_revision = cache.full_revision.wrapping_add(1);
        cache.entries.clear();
        cache.revisions.clear();
    }

    /// Evict the one cached archive addressed by a physical watcher path.
    ///
    /// Watchers observe the archive file rather than its projected entries, so a path ending in
    /// `.zip` identifies the cache key directly. An entry path is also accepted for callers that
    /// already have a physical projection. Unrelated paths are ignored.
    pub(crate) fn invalidate_archive(&self, physical_path: &Path) -> bool {
        let path = normalize(physical_path);
        let archive = match split_zip(&path) {
            Some((archive, _)) => archive,
            None if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_zip_name) =>
            {
                path
            }
            None => return false,
        };
        let mut cache = self.archive_cache.lock().unwrap();
        if let Some(revision) = cache.revisions.get_mut(&archive) {
            *revision = revision.wrapping_add(1);
        }
        cache.entries.remove(&archive).is_some()
    }

    /// 打开（或复用缓存）一个 zip 归档。
    fn open_archive(&self, zip_path: &Path) -> io::Result<Arc<ZipArchive>> {
        let key = normalize(zip_path);
        loop {
            let (observed_full_revision, observed_archive_revision) = {
                let mut cache = self.archive_cache.lock().unwrap();
                if let Some(archive) = cache.entries.get(&key) {
                    return Ok(archive.clone());
                }
                let full_revision = cache.full_revision;
                let archive_revision = *cache.revisions.entry(key.clone()).or_default();
                (full_revision, archive_revision)
            };

            // 不持锁读盘+解析（zip 可达数 MB），避免序列化所有首次打开；竞态下可重解一次。
            let parsed = self
                .inner
                .read(zip_path)
                .and_then(ZipArchive::parse)
                .map(Arc::new);
            let mut cache = self.archive_cache.lock().unwrap();
            if cache.full_revision != observed_full_revision
                || cache.revisions.get(&key).copied().unwrap_or_default()
                    != observed_archive_revision
            {
                // A watcher invalidation linearized while I/O was in flight. Discard both stale
                // bytes and stale errors, then reopen against the new filesystem generation.
                continue;
            }
            let archive = parsed?;
            return Ok(cache.entries.entry(key.clone()).or_insert(archive).clone());
        }
    }
}

/// 若物理路径含 `*.zip` 段，拆成（归档路径, 归档内路径）。zip 为末段（无内部路径）→ `None`（当普通文件）。
fn split_zip(path: &Path) -> Option<(PathBuf, String)> {
    let comps: Vec<Component> = path.components().collect();
    for (i, c) in comps.iter().enumerate() {
        if let Component::Normal(s) = c
            && s.to_str().map(is_zip_name).unwrap_or(false)
        {
            // 归档 = comps[0..=i]，内部 = comps[i+1..]（正斜杠拼接）。
            if i + 1 >= comps.len() {
                return None; // zip 本身作为末段——当普通文件走内层 FS
            }
            let mut archive = PathBuf::new();
            for cc in &comps[0..=i] {
                archive.push(cc.as_os_str());
            }
            let inner: Vec<String> = comps[i + 1..]
                .iter()
                .filter_map(|cc| cc.as_os_str().to_str().map(str::to_string))
                .collect();
            return Some((archive, inner.join("/")));
        }
    }
    None
}

fn is_zip_name(name: &str) -> bool {
    name.len() > 4 && name[name.len() - 4..].eq_ignore_ascii_case(".zip")
}

impl FileSystem for PnpFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let logical = normalize(path);
        let physical = self.physical(&logical);
        if let Some((archive, inner)) = split_zip(&physical) {
            let archive = self.open_archive(&archive)?;
            if archive.is_file(&inner) || archive.is_dir(&inner) {
                return Ok(logical);
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                path.display().to_string(),
            ));
        }

        let canonical = self.inner.canonicalize(&physical)?;
        if physical == logical {
            Ok(canonical)
        } else {
            Ok(logical)
        }
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let physical = self.physical(path);
        match split_zip(&physical) {
            Some((zip, inner)) => {
                let archive = self.open_archive(&zip)?;
                let bytes = archive.read(&inner)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{}!{inner}", zip.display()),
                    )
                })?;
                String::from_utf8(bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            }
            None => self.inner.read_to_string(&physical),
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let phys = self.physical(path);
        match split_zip(&phys) {
            Some((zip, inner)) => {
                let arc = self.open_archive(&zip)?;
                arc.read(&inner)?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("{}!{inner}", zip.display()),
                    )
                })
            }
            None => self.inner.read(&phys),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        let phys = self.physical(path);
        match split_zip(&phys) {
            Some((zip, inner)) => self
                .open_archive(&zip)
                .map(|a| a.is_file(&inner))
                .unwrap_or(false),
            None => self.inner.is_file(&phys),
        }
    }

    fn is_dir(&self, path: &Path) -> bool {
        let phys = self.physical(path);
        match split_zip(&phys) {
            Some((zip, inner)) => self
                .open_archive(&zip)
                .map(|a| a.is_dir(&inner))
                .unwrap_or(false),
            None => self.inner.is_dir(&phys),
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let logical = normalize(path);
        let phys = self.physical(&logical);
        match split_zip(&phys) {
            Some((zip, inner)) => {
                let arc = self.open_archive(&zip)?;
                let logical_zip = split_zip(&logical)
                    .map(|(archive, _)| archive)
                    .unwrap_or(zip);
                Ok(arc
                    .read_dir(&inner)
                    .into_iter()
                    .map(|child| {
                        // Keep the module identity at the caller's logical (possibly virtual)
                        // archive path; only I/O and watching use the physical projection.
                        logical_zip.join(child.trim_end_matches('/'))
                    })
                    .collect())
            }
            None => self.inner.read_dir(&phys),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    struct DistinctReadFamilyFileSystem;

    impl FileSystem for DistinctReadFamilyFileSystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(normalize(path))
        }

        fn read_to_string(&self, _path: &Path) -> io::Result<String> {
            Ok("text-family".to_owned())
        }

        fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
            Ok(b"byte-family".to_vec())
        }

        fn exists(&self, _path: &Path) -> bool {
            true
        }

        fn is_file(&self, _path: &Path) -> bool {
            true
        }

        fn is_dir(&self, _path: &Path) -> bool {
            false
        }

        fn read_dir(&self, _path: &Path) -> io::Result<Vec<PathBuf>> {
            Ok(Vec::new())
        }
    }

    struct PausingArchiveFileSystem {
        archive: Mutex<Vec<u8>>,
        first_read: AtomicBool,
        started: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
    }

    impl FileSystem for PausingArchiveFileSystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            Ok(normalize(path))
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            String::from_utf8(self.read(path)?)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
        }

        fn read(&self, _path: &Path) -> io::Result<Vec<u8>> {
            let bytes = self.archive.lock().unwrap().clone();
            if !self.first_read.swap(true, Ordering::SeqCst) {
                self.started.send(()).unwrap();
                self.resume.lock().unwrap().recv().unwrap();
            }
            Ok(bytes)
        }

        fn exists(&self, _path: &Path) -> bool {
            true
        }

        fn is_file(&self, _path: &Path) -> bool {
            true
        }

        fn is_dir(&self, _path: &Path) -> bool {
            false
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                path.display().to_string(),
            ))
        }
    }

    pub(crate) fn one_entry_zip(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(contents);

        let central_directory_start = bytes.len() as u32;
        bytes.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(contents.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&(name.len() as u16).to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(name.as_bytes());
        let central_directory_size = bytes.len() as u32 - central_directory_start;

        bytes.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&[0; 2]);
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&central_directory_size.to_le_bytes());
        bytes.extend_from_slice(&central_directory_start.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes
    }

    #[test]
    fn split_zip_detects_archive_boundary() {
        let (zip, inner) =
            split_zip(Path::new("../cache/react.zip/node_modules/react/index.js")).unwrap();
        assert_eq!(zip, PathBuf::from("../cache/react.zip"));
        assert_eq!(inner, "node_modules/react/index.js");
        // zip 作为末段 → None（当普通文件）。
        assert!(split_zip(Path::new("../cache/react.zip")).is_none());
        // 无 zip 段 → None。
        assert!(split_zip(Path::new("src/index.ts")).is_none());
    }

    #[test]
    fn ordinary_text_reads_preserve_the_inner_query_family() {
        let fs = PnpFileSystem::new(Arc::new(DistinctReadFamilyFileSystem));

        assert_eq!(
            fs.read_to_string(Path::new("src/index.js")).unwrap(),
            "text-family"
        );
        assert_eq!(fs.read(Path::new("src/index.js")).unwrap(), b"byte-family");
    }

    #[test]
    fn is_zip_name_cases() {
        assert!(is_zip_name("foo.zip"));
        assert!(is_zip_name("a.ZIP"));
        assert!(!is_zip_name(".zip"));
        assert!(!is_zip_name("zip"));
        assert!(!is_zip_name("foo.zipx"));
    }

    #[test]
    fn virtual_zip_paths_project_to_one_physical_watch_archive() {
        let fs = PnpFileSystem::new(Arc::new(wake_common::MemoryFileSystem::new()));
        let logical = Path::new(
            ".yarn/__virtual__/react-virtual/0/cache/react.zip/node_modules/react/index.js",
        );
        assert_eq!(
            fs.physical_path(logical),
            PathBuf::from(".yarn/cache/react.zip/node_modules/react/index.js")
        );
        assert_eq!(
            fs.watch_path(logical),
            PathBuf::from(".yarn/cache/react.zip")
        );
    }

    #[test]
    fn virtual_zip_read_dir_preserves_logical_path_identity() {
        let inner = Arc::new(wake_common::MemoryFileSystem::new());
        inner.insert(
            ".yarn/cache/react.zip",
            one_entry_zip("node_modules/react/index.js", b"react"),
        );
        let fs = PnpFileSystem::new(inner);
        let logical =
            Path::new(".yarn/__virtual__/react-virtual/0/cache/react.zip/node_modules/react");
        let logical_file = logical.join("index.js");

        assert_eq!(fs.read_dir(logical).unwrap(), vec![logical_file.clone()]);
        assert_eq!(fs.canonicalize(logical).unwrap(), logical);
        assert_eq!(fs.canonicalize(&logical_file).unwrap(), logical_file);
        assert_eq!(
            fs.canonicalize(&logical.join("missing.js"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn invalidation_wins_against_an_in_flight_archive_open() {
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let inner = Arc::new(PausingArchiveFileSystem {
            archive: Mutex::new(one_entry_zip("pkg/index.js", b"v1")),
            first_read: AtomicBool::new(false),
            started: started_tx,
            resume: Mutex::new(resume_rx),
        });
        let fs = Arc::new(PnpFileSystem::new(inner.clone()));
        let reader = {
            let fs = Arc::clone(&fs);
            std::thread::spawn(move || fs.read(Path::new("cache/pkg.zip/pkg/index.js")).unwrap())
        };

        started_rx.recv().unwrap();
        *inner.archive.lock().unwrap() = one_entry_zip("pkg/index.js", b"v2");
        fs.invalidate_archive(Path::new("cache/pkg.zip"));
        resume_tx.send(()).unwrap();

        assert_eq!(reader.join().unwrap(), b"v2");
    }
}
