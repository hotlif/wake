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
    /// 归档路径（归一化）→ 已解析归档。首次访问某 zip 时读盘+解目录，之后复用。
    archives: Mutex<FxHashMap<PathBuf, Arc<ZipArchive>>>,
}

impl PnpFileSystem {
    pub fn new(inner: Arc<dyn FileSystem>) -> PnpFileSystem {
        PnpFileSystem {
            inner,
            archives: Mutex::new(FxHashMap::default()),
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
        self.archives.lock().unwrap().clear();
    }

    /// 打开（或复用缓存）一个 zip 归档。
    fn open_archive(&self, zip_path: &Path) -> io::Result<Arc<ZipArchive>> {
        let key = normalize(zip_path);
        if let Some(a) = self.archives.lock().unwrap().get(&key) {
            return Ok(a.clone());
        }
        // 不持锁读盘+解析（zip 可达数 MB），避免序列化所有首次打开；竞态下可能重解一次，无害。
        let bytes = self.inner.read(zip_path)?;
        let arc = Arc::new(ZipArchive::parse(bytes)?);
        let mut map = self.archives.lock().unwrap();
        Ok(map.entry(key).or_insert(arc).clone())
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
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
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
        let phys = self.physical(path);
        match split_zip(&phys) {
            Some((zip, inner)) => {
                let arc = self.open_archive(&zip)?;
                Ok(arc
                    .read_dir(&inner)
                    .into_iter()
                    .map(|child| {
                        // 子项内部路径 → 完整（含 zip 前缀）的逻辑路径。
                        zip.join(child.trim_end_matches('/'))
                    })
                    .collect())
            }
            None => self.inner.read_dir(&phys),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
