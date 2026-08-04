//! 文件系统抽象：编译核心只依赖 [`FileSystem`] trait，测试用内存 FS，生产用 OS FS（DESIGN §3.3）。
//!
//! 第一版是同步阻塞接口——编译负载下顺序读很快，不值得把 async 引入编译核心
//! （DESIGN §5.2）。dev server 的网络层才用 tokio。

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rustc_hash::FxHashMap;

/// 抽象文件系统。`Send + Sync` 以便在工作窃取执行器的多线程间共享（`Arc<dyn FileSystem>`）。
pub trait FileSystem: Send + Sync {
    /// 读文件为 UTF-8 字符串。
    fn read_to_string(&self, path: &Path) -> io::Result<String>;

    /// 读文件为字节。
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;

    /// 路径是否存在。
    fn exists(&self, path: &Path) -> bool;

    /// 是否是文件。
    fn is_file(&self, path: &Path) -> bool;

    /// 是否是目录。
    fn is_dir(&self, path: &Path) -> bool;

    /// 列目录直接子项（resolver 的目录级批量 listing 缓存基础，DESIGN §5.1）。
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
}

/// 真实操作系统文件系统。
#[derive(Debug, Default, Clone, Copy)]
pub struct OsFileSystem;

impl FileSystem for OsFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn is_dir(&self, path: &Path) -> bool {
        path.is_dir()
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(path)? {
            out.push(entry?.path());
        }
        Ok(out)
    }
}

/// 内存文件系统，供单测 / fixture 使用（DESIGN §3.3：测试可用内存 FS）。
///
/// 路径以 [`normalize`] 规范化后作为键，因此 `a/./b` 与 `a/b` 等价。
#[derive(Default)]
pub struct MemoryFileSystem {
    files: Mutex<FxHashMap<PathBuf, Vec<u8>>>,
}

impl MemoryFileSystem {
    pub fn new() -> MemoryFileSystem {
        MemoryFileSystem::default()
    }

    /// 写入 / 覆盖一个文件。
    pub fn insert(&self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) {
        let key = normalize(path.as_ref());
        self.files.lock().unwrap().insert(key, contents.into());
    }

    /// 便捷构造：从 `(路径, 内容)` 列表建立。
    pub fn from_files<P, C>(entries: impl IntoIterator<Item = (P, C)>) -> MemoryFileSystem
    where
        P: AsRef<Path>,
        C: Into<Vec<u8>>,
    {
        let fs = MemoryFileSystem::new();
        for (p, c) in entries {
            fs.insert(p, c);
        }
        fs
    }

    fn dir_has_children(&self, dir: &Path) -> bool {
        let dir = normalize(dir);
        self.files.lock().unwrap().keys().any(|k| {
            k.parent().map(normalize).as_deref() == Some(&dir) || k.starts_with(&dir) && k != &dir
        })
    }
}

impl FileSystem for MemoryFileSystem {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let bytes = self.read(path)?;
        String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let key = normalize(path);
        self.files
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{}", path.display())))
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        let key = normalize(path);
        self.files.lock().unwrap().contains_key(&key)
    }

    fn is_dir(&self, path: &Path) -> bool {
        !self.is_file(path) && self.dir_has_children(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let dir = normalize(path);
        let files = self.files.lock().unwrap();
        let mut children: Vec<PathBuf> = Vec::new();
        for key in files.keys() {
            if let Some(parent) = key.parent()
                && normalize(parent) == dir
            {
                children.push(key.clone());
            }
        }
        if children.is_empty() && !self.dir_has_children(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}", path.display()),
            ));
        }
        children.sort();
        Ok(children)
    }
}

/// 轻量路径规范化：折叠 `.`、消解 `..`、统一分隔符为 `/`。
///
/// **不** 触碰文件系统（不解析 symlink，不判大小写）——真正的规范化在 resolver
/// （DESIGN §5.1）里做，这里只为内存 FS 的键稳定。
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            #[cfg(windows)]
            Component::Prefix(prefix) => {
                use std::ffi::OsString;
                use std::path::Prefix;

                match prefix.kind() {
                    Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => {
                        let mut disk = OsString::from(char::from(drive).to_string());
                        disk.push(":");
                        out.push(disk);
                    }
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        let mut unc = OsString::from(r"\\");
                        unc.push(server);
                        unc.push(r"\");
                        unc.push(share);
                        out.push(unc);
                    }
                    _ => out.push(prefix.as_os_str()),
                }
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // 仅当末段是真实目录名（Normal）时才消解；否则（空 / 末段已是 `..` / 根）
                // 累积 `..`——否则连续前导 `../..` 会互相弹出，把 `../../x` 误缩成 `x`。
                match out.components().next_back() {
                    Some(Component::Normal(_)) => {
                        out.pop();
                    }
                    _ => out.push(".."),
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_fs_read_write() {
        let fs = MemoryFileSystem::new();
        fs.insert("src/index.js", "let x = 1;");
        assert!(fs.is_file(Path::new("src/index.js")));
        assert_eq!(
            fs.read_to_string(Path::new("src/index.js")).unwrap(),
            "let x = 1;"
        );
        assert!(fs.read(Path::new("nope.js")).is_err());
    }

    #[test]
    fn memory_fs_normalizes_keys() {
        let fs = MemoryFileSystem::new();
        fs.insert("src/./a.js", "a");
        assert!(fs.is_file(Path::new("src/a.js")));
        assert_eq!(
            fs.read_to_string(Path::new("src/foo/../a.js")).unwrap(),
            "a"
        );
    }

    #[test]
    fn memory_fs_dirs_and_listing() {
        let fs = MemoryFileSystem::from_files([
            ("pkg/a.js", "a"),
            ("pkg/b.js", "b"),
            ("pkg/sub/c.js", "c"),
        ]);
        assert!(fs.is_dir(Path::new("pkg")));
        assert!(!fs.is_file(Path::new("pkg")));
        let mut listing = fs.read_dir(Path::new("pkg")).unwrap();
        listing.sort();
        assert_eq!(
            listing,
            vec![PathBuf::from("pkg/a.js"), PathBuf::from("pkg/b.js")]
        );
    }

    #[test]
    fn normalize_cases() {
        assert_eq!(normalize(Path::new("a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize(Path::new("a/b/../c")), PathBuf::from("a/c"));
        assert_eq!(normalize(Path::new("../x")), PathBuf::from("../x"));
        // 连续前导 `..` 必须累积，不能互相弹出（PnP 的 `../../../../../../cache` 依赖此）。
        assert_eq!(
            normalize(Path::new("../../../cache/x")),
            PathBuf::from("../../../cache/x")
        );
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));

        #[cfg(windows)]
        {
            assert_eq!(
                normalize(Path::new(r"\\?\C:\work\src\index.js")),
                normalize(Path::new(r"C:\work\src\index.js"))
            );
            assert_eq!(
                normalize(Path::new(r"\\?\UNC\server\share\src\index.js")),
                normalize(Path::new(r"\\server\share\src\index.js"))
            );
        }
    }
}
