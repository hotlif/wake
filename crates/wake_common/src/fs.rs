//! 文件系统抽象：编译核心只依赖 [`FileSystem`] trait，测试用内存 FS，生产用 OS FS（DESIGN §3.3）。
//!
//! 第一版是同步阻塞接口——编译负载下顺序读很快，不值得把 async 引入编译核心
//! （DESIGN §5.2）。dev server 的网络层才用 tokio。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rustc_hash::FxHashMap;

/// 抽象文件系统。`Send + Sync` 以便在工作窃取执行器的多线程间共享（`Arc<dyn FileSystem>`）。
pub trait FileSystem: Send + Sync {
    /// Resolve one stable path identity in this filesystem's logical namespace.
    ///
    /// Virtual and projected implementations must not leak or consult a shadowed physical path.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;

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

/// A validated, normalized path relative to an owned projection root.
///
/// Unlike [`normalize`], construction is intentionally strict: absolute paths, empty paths,
/// prefixes, roots, and explicit `.` or `..` components are rejected instead of rewritten.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectedRelativePath(PathBuf);

impl ProjectedRelativePath {
    pub fn new(path: impl AsRef<Path>) -> Result<Self, OwnedFileTreeError> {
        let path = path.as_ref();
        if path.as_os_str().is_empty() || contains_explicit_dot_component(path) {
            return Err(OwnedFileTreeError::InvalidRelativePath {
                path: path.to_path_buf(),
            });
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(name) => normalized.push(name),
                _ => {
                    return Err(OwnedFileTreeError::InvalidRelativePath {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
        if normalized.as_os_str().is_empty() {
            return Err(OwnedFileTreeError::InvalidRelativePath {
                path: path.to_path_buf(),
            });
        }
        Ok(Self(normalized))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for ProjectedRelativePath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl TryFrom<PathBuf> for ProjectedRelativePath {
    type Error = OwnedFileTreeError;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl TryFrom<&Path> for ProjectedRelativePath {
    type Error = OwnedFileTreeError;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        Self::new(path)
    }
}

impl fmt::Display for ProjectedRelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// Validation failures raised before an immutable owned file tree is sealed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedFileTreeError {
    InvalidRelativePath {
        path: PathBuf,
    },
    InvalidLogicalRoot {
        path: PathBuf,
    },
    DuplicatePath {
        path: PathBuf,
    },
    CaseEquivalentPath {
        existing: PathBuf,
        candidate: PathBuf,
    },
    FileDirectoryConflict {
        file: PathBuf,
        descendant: PathBuf,
    },
}

impl fmt::Display for OwnedFileTreeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRelativePath { path } => write!(
                formatter,
                "owned file path must contain only normal relative components: {}",
                path.display()
            ),
            Self::InvalidLogicalRoot { path } => write!(
                formatter,
                "owned overlay root must be a non-root path without dot components: {}",
                path.display()
            ),
            Self::DuplicatePath { path } => {
                write!(formatter, "duplicate owned file path: {}", path.display())
            }
            Self::CaseEquivalentPath {
                existing,
                candidate,
            } => write!(
                formatter,
                "case-equivalent owned paths are not allowed: {} and {}",
                existing.display(),
                candidate.display()
            ),
            Self::FileDirectoryConflict { file, descendant } => write!(
                formatter,
                "owned path is both a file and a directory: {} conflicts with {}",
                file.display(),
                descendant.display()
            ),
        }
    }
}

impl std::error::Error for OwnedFileTreeError {}

/// Single-use builder for an immutable set of owned virtual files.
///
/// The builder is deliberately not [`Clone`]. Call [`Self::seal`] exactly once to transfer every
/// byte into a cloneable [`OwnedFileTree`].
#[derive(Debug, Default)]
pub struct OwnedFileTreeBuilder {
    files: BTreeMap<ProjectedRelativePath, Arc<[u8]>>,
}

impl OwnedFileTreeBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(
        &mut self,
        path: ProjectedRelativePath,
        contents: impl Into<Arc<[u8]>>,
    ) -> Result<(), OwnedFileTreeError> {
        for existing in self.files.keys() {
            if paths_equal_platform(existing.as_path(), path.as_path()) {
                return if existing == &path {
                    Err(OwnedFileTreeError::DuplicatePath {
                        path: path.into_path_buf(),
                    })
                } else {
                    Err(OwnedFileTreeError::CaseEquivalentPath {
                        existing: existing.as_path().to_path_buf(),
                        candidate: path.into_path_buf(),
                    })
                };
            }

            if path_starts_with_platform(path.as_path(), existing.as_path()) {
                return Err(OwnedFileTreeError::FileDirectoryConflict {
                    file: existing.as_path().to_path_buf(),
                    descendant: path.into_path_buf(),
                });
            }
            if path_starts_with_platform(existing.as_path(), path.as_path()) {
                return Err(OwnedFileTreeError::FileDirectoryConflict {
                    file: path.into_path_buf(),
                    descendant: existing.as_path().to_path_buf(),
                });
            }

            if path_has_case_equivalent_spelling_conflict(existing.as_path(), path.as_path()) {
                return Err(OwnedFileTreeError::CaseEquivalentPath {
                    existing: existing.as_path().to_path_buf(),
                    candidate: path.into_path_buf(),
                });
            }
        }

        self.files.insert(path, contents.into());
        Ok(())
    }

    pub fn seal(self) -> OwnedFileTree {
        OwnedFileTree {
            files: Arc::new(self.files),
        }
    }
}

/// Immutable, cloneable inventory of bytes owned by a logical generated-input tree.
#[derive(Debug, Clone)]
pub struct OwnedFileTree {
    files: Arc<BTreeMap<ProjectedRelativePath, Arc<[u8]>>>,
}

impl OwnedFileTree {
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn get(&self, path: &ProjectedRelativePath) -> Option<&[u8]> {
        self.find_file(path.as_path()).map(AsRef::as_ref)
    }

    /// Returns the immutable shared allocation for zero-copy composition into another owner.
    pub fn get_shared(&self, path: &ProjectedRelativePath) -> Option<&Arc<[u8]>> {
        self.find_file(path.as_path())
    }

    /// Returns paths in a stable, insertion-order-independent order.
    pub fn inventory(&self) -> impl ExactSizeIterator<Item = &ProjectedRelativePath> {
        self.files.keys()
    }

    /// Returns paths and immutable shared allocations in stable path order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&ProjectedRelativePath, &Arc<[u8]>)> {
        self.files.iter()
    }

    fn find_file(&self, path: &Path) -> Option<&Arc<[u8]>> {
        self.files
            .iter()
            .find(|(candidate, _)| paths_equal_platform(candidate.as_path(), path))
            .map(|(_, contents)| contents)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.find_file(path).is_none()
            && self.files.keys().any(|candidate| {
                candidate.as_path().components().count() > path.components().count()
                    && path_starts_with_platform(candidate.as_path(), path)
            })
    }

    fn has_file_ancestor(&self, path: &Path) -> bool {
        self.files.keys().any(|candidate| {
            candidate.as_path().components().count() < path.components().count()
                && path_starts_with_platform(path, candidate.as_path())
        })
    }

    fn read_dir(
        &self,
        path: &Path,
        logical_root: &Path,
        requested_path: &Path,
    ) -> io::Result<Vec<PathBuf>> {
        if self.find_file(path).is_some() || self.has_file_ancestor(path) {
            return Err(path_error(io::ErrorKind::NotADirectory, requested_path));
        }

        let depth = path.components().count();
        let children = self
            .files
            .keys()
            .filter(|candidate| {
                candidate.as_path().components().count() > depth
                    && path_starts_with_platform(candidate.as_path(), path)
            })
            .map(|candidate| {
                let relative = candidate
                    .as_path()
                    .components()
                    .take(depth + 1)
                    .map(|component| component.as_os_str())
                    .collect::<PathBuf>();
                logical_root.join(relative)
            })
            .collect::<BTreeSet<_>>();

        if children.is_empty() {
            Err(path_error(io::ErrorKind::NotFound, requested_path))
        } else {
            Ok(children.into_iter().collect())
        }
    }
}

/// A filesystem overlay that exclusively owns one immutable logical subtree.
///
/// Paths below `logical_root` are answered only from `tree`; undeclared files never fall through
/// to `base`. Outside that subtree the base filesystem keeps its original semantics.
#[derive(Clone)]
pub struct OwnedOverlayFileSystem {
    base: Arc<dyn FileSystem>,
    logical_root: PathBuf,
    tree: OwnedFileTree,
}

impl OwnedOverlayFileSystem {
    pub fn try_new(
        base: Arc<dyn FileSystem>,
        logical_root: impl Into<PathBuf>,
        tree: OwnedFileTree,
    ) -> Result<Self, OwnedFileTreeError> {
        let logical_root = logical_root.into();
        if !valid_logical_root(&logical_root) {
            return Err(OwnedFileTreeError::InvalidLogicalRoot { path: logical_root });
        }
        Ok(Self {
            base,
            logical_root: normalize(&logical_root),
            tree,
        })
    }

    pub fn logical_root(&self) -> &Path {
        &self.logical_root
    }

    pub fn tree(&self) -> &OwnedFileTree {
        &self.tree
    }

    fn owned_relative(&self, path: &Path) -> Option<PathBuf> {
        strip_prefix_platform(path, &self.logical_root)
    }

    fn synthetic_child(&self, path: &Path) -> Option<PathBuf> {
        if path.is_absolute() != self.logical_root.is_absolute()
            || paths_equal_platform(path, &self.logical_root)
            || !path_starts_with_platform(&self.logical_root, path)
        {
            return None;
        }
        Some(
            self.logical_root
                .components()
                .take(path.components().count() + 1)
                .map(|component| component.as_os_str())
                .collect(),
        )
    }

    fn read_owned(&self, path: &Path, relative: &Path) -> io::Result<Vec<u8>> {
        if let Some(contents) = self.tree.find_file(relative) {
            return Ok(contents.to_vec());
        }
        if self.tree.is_dir(relative) {
            Err(path_error(io::ErrorKind::IsADirectory, path))
        } else if self.tree.has_file_ancestor(relative) {
            Err(path_error(io::ErrorKind::NotADirectory, path))
        } else {
            Err(path_error(io::ErrorKind::NotFound, path))
        }
    }
}

impl FileSystem for OwnedOverlayFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let normalized = normalize(path);
        if let Some(relative) = self.owned_relative(&normalized) {
            if self.tree.find_file(&relative).is_some() || self.tree.is_dir(&relative) {
                return Ok(normalized);
            }
            return Err(path_error(
                if self.tree.has_file_ancestor(&relative) {
                    io::ErrorKind::NotADirectory
                } else {
                    io::ErrorKind::NotFound
                },
                path,
            ));
        }

        match self.base.canonicalize(path) {
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && self
                        .synthetic_child(&normalized)
                        .is_some_and(|_| !self.tree.is_empty()) =>
            {
                Ok(normalized)
            }
            result => result,
        }
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        let normalized = normalize(path);
        let Some(relative) = self.owned_relative(&normalized) else {
            return self.base.read_to_string(path);
        };
        let bytes = self.read_owned(path, &relative)?;
        String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let normalized = normalize(path);
        match self.owned_relative(&normalized) {
            Some(relative) => self.read_owned(path, &relative),
            None => self.base.read(path),
        }
    }

    fn exists(&self, path: &Path) -> bool {
        let normalized = normalize(path);
        if let Some(relative) = self.owned_relative(&normalized) {
            return self.tree.find_file(&relative).is_some() || self.tree.is_dir(&relative);
        }
        self.synthetic_child(&normalized)
            .is_some_and(|_| !self.tree.is_empty())
            || self.base.exists(path)
    }

    fn is_file(&self, path: &Path) -> bool {
        let normalized = normalize(path);
        if let Some(relative) = self.owned_relative(&normalized) {
            return self.tree.find_file(&relative).is_some();
        }
        self.base.is_file(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        let normalized = normalize(path);
        if let Some(relative) = self.owned_relative(&normalized) {
            return self.tree.is_dir(&relative);
        }
        if self
            .synthetic_child(&normalized)
            .is_some_and(|_| !self.tree.is_empty())
        {
            !self.base.is_file(path)
        } else {
            self.base.is_dir(path)
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let normalized = normalize(path);
        if let Some(relative) = self.owned_relative(&normalized) {
            return self.tree.read_dir(&relative, &self.logical_root, path);
        }

        let Some(synthetic_child) = self.synthetic_child(&normalized) else {
            return self.base.read_dir(path);
        };
        let mut children = match self.base.read_dir(path) {
            Ok(children) => children,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !self.tree.is_empty() => {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        children.retain(|child| !paths_equal_platform(&normalize(child), &synthetic_child));
        if !self.tree.is_empty() {
            children.push(synthetic_child);
        }
        children.sort();
        children.dedup();
        Ok(children)
    }
}

fn path_error(kind: io::ErrorKind, path: &Path) -> io::Error {
    io::Error::new(kind, format!("{}", path.display()))
}

fn valid_logical_root(path: &Path) -> bool {
    if path.as_os_str().is_empty() || contains_explicit_dot_component(path) {
        return false;
    }
    let mut saw_normal = false;
    let mut saw_prefix = false;
    for component in path.components() {
        match component {
            std::path::Component::Normal(_) => saw_normal = true,
            std::path::Component::Prefix(_) => saw_prefix = true,
            std::path::Component::RootDir => {}
            std::path::Component::CurDir | std::path::Component::ParentDir => return false,
        }
    }
    saw_normal && (!saw_prefix || path.is_absolute()) && (!path.has_root() || path.is_absolute())
}

fn contains_explicit_dot_component(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        path.as_os_str()
            .as_bytes()
            .split(|byte| *byte == b'/')
            .any(|component| component == b"." || component == b"..")
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let path = path.as_os_str().encode_wide().collect::<Vec<_>>();
        path.split(|unit| *unit == u16::from(b'/') || *unit == u16::from(b'\\'))
            .any(|component| {
                component == [u16::from(b'.')] || component == [u16::from(b'.'), u16::from(b'.')]
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    }
}

fn path_has_case_equivalent_spelling_conflict(left: &Path, right: &Path) -> bool {
    #[cfg(not(windows))]
    {
        let _ = (left, right);
        false
    }
    #[cfg(windows)]
    {
        for (left, right) in left.components().zip(right.components()) {
            let left = left.as_os_str().to_string_lossy();
            let right = right.as_os_str().to_string_lossy();
            if !left.eq_ignore_ascii_case(&right) {
                return false;
            }
            if left != right {
                return true;
            }
        }
        false
    }
}

/// Presents a caller-owned physical generation tree at a stable logical path.
///
/// Build identities must not contain random staging-directory names. The resolver therefore sees
/// `logical_root`, while reads are projected to `physical_root`. Directory listings are mapped
/// back to logical children so an absolute physical path never escapes into module identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSystemProjection {
    pub logical_root: PathBuf,
    pub physical_root: PathBuf,
    kind: FileSystemProjectionKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSystemProjectionKind {
    ExactFile,
    Tree,
}

fn join_projection_relative(root: &Path, relative: &Path) -> PathBuf {
    if relative.as_os_str().is_empty() {
        root.to_path_buf()
    } else {
        root.join(relative)
    }
}

impl FileSystemProjection {
    pub fn exact_file(logical: impl Into<PathBuf>, physical: impl Into<PathBuf>) -> Self {
        Self::with_kind(logical, physical, FileSystemProjectionKind::ExactFile)
    }

    pub fn tree(logical: impl Into<PathBuf>, physical: impl Into<PathBuf>) -> Self {
        Self::with_kind(logical, physical, FileSystemProjectionKind::Tree)
    }

    fn with_kind(
        logical_root: impl Into<PathBuf>,
        physical_root: impl Into<PathBuf>,
        kind: FileSystemProjectionKind,
    ) -> Self {
        Self {
            logical_root: normalize(&logical_root.into()),
            physical_root: normalize(&physical_root.into()),
            kind,
        }
    }

    fn contains_logical(&self, path: &Path) -> bool {
        match self.kind {
            FileSystemProjectionKind::ExactFile => paths_equal_platform(path, &self.logical_root),
            FileSystemProjectionKind::Tree => path_starts_with_platform(path, &self.logical_root),
        }
    }

    fn contains_physical(&self, path: &Path) -> bool {
        match self.kind {
            FileSystemProjectionKind::ExactFile => paths_equal_platform(path, &self.physical_root),
            FileSystemProjectionKind::Tree => path_starts_with_platform(path, &self.physical_root),
        }
    }

    pub fn to_physical(&self, path: &Path) -> PathBuf {
        let path = normalize(path);
        self.contains_logical(&path)
            .then(|| strip_prefix_platform(&path, &self.logical_root).expect("checked projection"))
            .map(|relative| join_projection_relative(&self.physical_root, &relative))
            .unwrap_or(path)
    }

    pub fn to_logical(&self, path: &Path) -> PathBuf {
        let path = normalize(path);
        self.contains_physical(&path)
            .then(|| strip_prefix_platform(&path, &self.physical_root).expect("checked projection"))
            .map(|relative| join_projection_relative(&self.logical_root, &relative))
            .unwrap_or(path)
    }
}

#[derive(Clone)]
pub struct ProjectedFileSystem {
    inner: Arc<dyn FileSystem>,
    projections: Vec<FileSystemProjection>,
    hidden_trees: Vec<(PathBuf, PathBuf)>,
    _lifetime_guard: Option<Arc<dyn Send + Sync>>,
}

impl ProjectedFileSystem {
    pub fn try_new(
        inner: Arc<dyn FileSystem>,
        projections: impl IntoIterator<Item = FileSystemProjection>,
    ) -> io::Result<Self> {
        let mut projections = projections.into_iter().collect::<Vec<_>>();
        projections.sort_by_key(|projection| {
            std::cmp::Reverse(projection.logical_root.components().count())
        });
        for (index, projection) in projections.iter().enumerate() {
            for other in &projections[index + 1..] {
                if paths_equal_platform(&projection.logical_root, &other.logical_root)
                    || path_starts_with_platform(&projection.logical_root, &other.logical_root)
                    || path_starts_with_platform(&other.logical_root, &projection.logical_root)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "overlapping logical filesystem projections",
                    ));
                }
                if paths_equal_platform(&projection.physical_root, &other.physical_root)
                    || path_starts_with_platform(&projection.physical_root, &other.physical_root)
                    || path_starts_with_platform(&other.physical_root, &projection.physical_root)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "overlapping physical filesystem projections",
                    ));
                }
            }
        }
        Ok(Self {
            inner,
            projections,
            hidden_trees: Vec::new(),
            _lifetime_guard: None,
        })
    }

    /// Keep the owner of projected physical paths alive for every clone of this filesystem.
    pub fn with_lifetime_guard(mut self, guard: Arc<dyn Send + Sync>) -> Self {
        self._lifetime_guard = Some(guard);
        self
    }

    /// Hide an internal physical namespace from unprojected logical reads and listings.
    pub fn with_hidden_tree(mut self, path: impl Into<PathBuf>) -> Self {
        let declared = normalize(&path.into());
        let resolved = resolve_existing_prefix(&declared);
        self.hidden_trees.push((declared, resolved));
        self.hidden_trees.sort();
        self.hidden_trees.dedup();
        self
    }

    fn is_hidden(&self, path: &Path) -> bool {
        let path = normalize(path);
        let resolved = resolve_existing_prefix(&path);
        self.hidden_trees.iter().any(|(declared, physical)| {
            path_starts_with_platform(&path, declared)
                || path_starts_with_platform(&resolved, physical)
        })
    }

    fn hidden_listing_entry(&self, path: &Path) -> bool {
        let path = normalize(path);
        let resolved = resolve_existing_prefix(&path);
        self.hidden_trees.iter().any(|(declared, physical)| {
            path_starts_with_platform(&path, declared)
                || path_starts_with_platform(&resolved, physical)
        })
    }

    fn hidden_error(path: &Path) -> io::Error {
        io::Error::new(io::ErrorKind::NotFound, format!("{}", path.display()))
    }

    fn projected(&self, path: &Path) -> Option<(&FileSystemProjection, PathBuf)> {
        let path = normalize(path);
        self.projections.iter().find_map(|projection| {
            projection
                .contains_logical(&path)
                .then(|| strip_prefix_platform(&path, &projection.logical_root))
                .flatten()
                .map(|relative| {
                    (
                        projection,
                        join_projection_relative(&projection.physical_root, &relative),
                    )
                })
        })
    }

    fn projection_children(&self, path: &Path) -> Vec<PathBuf> {
        let path = normalize(path);
        let mut children = self
            .projections
            .iter()
            .filter(|projection| self.inner.exists(&projection.physical_root))
            .filter_map(|projection| {
                let relative = strip_prefix_platform(&projection.logical_root, &path)?;
                let first = relative.components().next()?;
                Some(path.join(first.as_os_str()))
            })
            .collect::<Vec<_>>();
        children.sort();
        children.dedup();
        children
    }
}

fn resolve_existing_prefix(path: &Path) -> PathBuf {
    if let Ok(path) = path.canonicalize() {
        return normalize(&path);
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        let Some(name) = ancestor.file_name() else {
            return normalize(path);
        };
        suffix.push(name.to_os_string());
        let Some(parent) = ancestor.parent() else {
            return normalize(path);
        };
        ancestor = parent;
    }
    let mut resolved = ancestor
        .canonicalize()
        .map(|path| normalize(&path))
        .unwrap_or_else(|_| normalize(ancestor));
    for name in suffix.iter().rev() {
        resolved.push(name);
    }
    resolved
}

fn path_starts_with_platform(path: &Path, root: &Path) -> bool {
    #[cfg(not(windows))]
    {
        path.starts_with(root)
    }
    #[cfg(windows)]
    {
        let mut path = path.components();
        for root_component in root.components() {
            let Some(path_component) = path.next() else {
                return false;
            };
            if !path_component
                .as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(&root_component.as_os_str().to_string_lossy())
            {
                return false;
            }
        }
        true
    }
}

fn paths_equal_platform(left: &Path, right: &Path) -> bool {
    left.components().count() == right.components().count()
        && path_starts_with_platform(left, right)
}

fn strip_prefix_platform(path: &Path, root: &Path) -> Option<PathBuf> {
    if !path_starts_with_platform(path, root) {
        return None;
    }
    Some(
        path.components()
            .skip(root.components().count())
            .map(|component| component.as_os_str())
            .collect(),
    )
}

impl FileSystem for ProjectedFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let normalized = normalize(path);
        if let Some((_, physical)) = self.projected(&normalized) {
            self.inner.canonicalize(&physical)?;
            return Ok(normalized);
        }
        if self.is_hidden(path) {
            return Err(Self::hidden_error(path));
        }

        match self.inner.canonicalize(path) {
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && !self.projection_children(&normalized).is_empty() =>
            {
                Ok(normalized)
            }
            result => result,
        }
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        if let Some((_, physical)) = self.projected(path) {
            self.inner.read_to_string(&physical)
        } else if self.is_hidden(path) {
            Err(Self::hidden_error(path))
        } else {
            self.inner.read_to_string(path)
        }
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if let Some((_, physical)) = self.projected(path) {
            self.inner.read(&physical)
        } else if self.is_hidden(path) {
            Err(Self::hidden_error(path))
        } else {
            self.inner.read(path)
        }
    }

    fn exists(&self, path: &Path) -> bool {
        self.projected(path)
            .map(|(_, path)| self.inner.exists(&path))
            .unwrap_or_else(|| {
                let children = self.projection_children(path);
                !children.is_empty() || (!self.is_hidden(path) && self.inner.exists(path))
            })
    }

    fn is_file(&self, path: &Path) -> bool {
        self.projected(path)
            .map(|(_, path)| self.inner.is_file(&path))
            .unwrap_or_else(|| !self.is_hidden(path) && self.inner.is_file(path))
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.projected(path)
            .map(|(_, path)| self.inner.is_dir(&path))
            .unwrap_or_else(|| {
                let children = self.projection_children(path);
                !self.inner.is_file(path)
                    && (!children.is_empty() || (!self.is_hidden(path) && self.inner.is_dir(path)))
            })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        if let Some((projection, physical)) = self.projected(path) {
            return self.inner.read_dir(&physical).map(|paths| {
                let mut paths = paths
                    .into_iter()
                    .map(|path| {
                        let path = normalize(&path);
                        if !path
                            .parent()
                            .is_some_and(|parent| paths_equal_platform(parent, &physical))
                        {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                "projected filesystem returned a non-child directory entry",
                            ));
                        }
                        Ok(projection.to_logical(&path))
                    })
                    .collect::<io::Result<Vec<_>>>()?;
                paths.sort();
                paths.dedup();
                Ok(paths)
            })?;
        }
        if self.is_hidden(path) {
            let children = self.projection_children(path);
            return if children.is_empty() {
                Err(Self::hidden_error(path))
            } else {
                Ok(children)
            };
        }
        let mut paths = match self.inner.read_dir(path) {
            Ok(paths) => paths,
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    && !self.projection_children(path).is_empty() =>
            {
                Vec::new()
            }
            Err(error) => return Err(error),
        };
        paths.retain(|path| !self.hidden_listing_entry(path));
        paths.extend(self.projection_children(path));
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// 真实操作系统文件系统。
#[derive(Debug, Default, Clone, Copy)]
pub struct OsFileSystem;

impl FileSystem for OsFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path).map(|path| normalize(&path))
    }

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
}

impl FileSystem for MemoryFileSystem {
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let normalized = normalize(path);
        if self.exists(&normalized) {
            Ok(normalized)
        } else {
            Err(path_error(io::ErrorKind::NotFound, path))
        }
    }

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
        let dir = normalize(path);
        let files = self.files.lock().unwrap();
        !files.contains_key(&dir) && files.keys().any(|key| key != &dir && key.starts_with(&dir))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let dir = normalize(path);
        let files = self.files.lock().unwrap();

        if files.contains_key(&dir) {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{}", path.display()),
            ));
        }

        let mut directory_exists = false;
        let mut children = std::collections::BTreeSet::new();
        for key in files.keys() {
            let Ok(relative) = key.strip_prefix(&dir) else {
                continue;
            };
            let Some(first) = relative.components().next() else {
                continue;
            };
            directory_exists = true;
            children.insert(dir.join(first.as_os_str()));
        }

        if !directory_exists {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{}", path.display()),
            ));
        }

        Ok(children.into_iter().collect())
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

    struct OwnedRootPanicFileSystem {
        forbidden_root: PathBuf,
    }

    impl OwnedRootPanicFileSystem {
        fn reject_owned_path(&self, path: &Path) {
            assert!(
                !path_starts_with_platform(&normalize(path), &self.forbidden_root),
                "base filesystem was accessed inside owned root: {}",
                path.display()
            );
        }
    }

    impl FileSystem for OwnedRootPanicFileSystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            self.reject_owned_path(path);
            Err(path_error(io::ErrorKind::NotFound, path))
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            self.reject_owned_path(path);
            Err(path_error(io::ErrorKind::NotFound, path))
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.reject_owned_path(path);
            Err(path_error(io::ErrorKind::NotFound, path))
        }

        fn exists(&self, path: &Path) -> bool {
            self.reject_owned_path(path);
            false
        }

        fn is_file(&self, path: &Path) -> bool {
            self.reject_owned_path(path);
            false
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.reject_owned_path(path);
            false
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.reject_owned_path(path);
            Err(path_error(io::ErrorKind::NotFound, path))
        }
    }

    struct ExactDelegationFileSystem;

    impl ExactDelegationFileSystem {
        fn assert_original_path(path: &Path) {
            assert_eq!(path, Path::new("outside/./entry"));
        }
    }

    impl FileSystem for ExactDelegationFileSystem {
        fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
            Self::assert_original_path(path);
            Ok(PathBuf::from("base-canonical"))
        }

        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            Self::assert_original_path(path);
            Ok("base-string-method".to_string())
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            Self::assert_original_path(path);
            Ok(b"base-read-method".to_vec())
        }

        fn exists(&self, path: &Path) -> bool {
            Self::assert_original_path(path);
            true
        }

        fn is_file(&self, path: &Path) -> bool {
            Self::assert_original_path(path);
            true
        }

        fn is_dir(&self, path: &Path) -> bool {
            Self::assert_original_path(path);
            true
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            Self::assert_original_path(path);
            Ok(vec![PathBuf::from("base-listing")])
        }
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "wake-owned-overlay-{label}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn projected_path(path: impl AsRef<Path>) -> ProjectedRelativePath {
        ProjectedRelativePath::new(path).unwrap()
    }

    fn owned_tree(entries: &[(&str, &str)]) -> OwnedFileTree {
        let mut builder = OwnedFileTreeBuilder::new();
        for (path, contents) in entries {
            builder
                .insert(projected_path(path), contents.as_bytes())
                .unwrap();
        }
        builder.seal()
    }

    #[test]
    fn projected_relative_path_rejects_non_normal_components() {
        let invalid = [
            PathBuf::new(),
            PathBuf::from("."),
            PathBuf::from("./entry.ts"),
            PathBuf::from("nested/./entry.ts"),
            PathBuf::from(".."),
            PathBuf::from("../entry.ts"),
            PathBuf::from("nested/../entry.ts"),
            std::env::current_dir().unwrap().join("entry.ts"),
        ];
        for path in invalid {
            assert!(matches!(
                ProjectedRelativePath::new(&path),
                Err(OwnedFileTreeError::InvalidRelativePath { .. })
            ));
        }

        #[cfg(windows)]
        for path in [Path::new(r"C:entry.ts"), Path::new(r"\entry.ts")] {
            assert!(matches!(
                ProjectedRelativePath::new(path),
                Err(OwnedFileTreeError::InvalidRelativePath { .. })
            ));
        }

        assert_eq!(
            ProjectedRelativePath::new("nested//entry.ts")
                .unwrap()
                .as_path(),
            Path::new("nested/entry.ts")
        );
    }

    #[test]
    fn owned_tree_rejects_duplicates_and_file_directory_collisions() {
        let mut duplicate = OwnedFileTreeBuilder::new();
        duplicate
            .insert(projected_path("entry.ts"), &b"one"[..])
            .unwrap();
        assert!(matches!(
            duplicate.insert(projected_path("entry.ts"), &b"two"[..]),
            Err(OwnedFileTreeError::DuplicatePath { .. })
        ));

        let mut file_first = OwnedFileTreeBuilder::new();
        file_first
            .insert(projected_path("pages"), &b"file"[..])
            .unwrap();
        assert!(matches!(
            file_first.insert(projected_path("pages/index.ts"), &b"child"[..]),
            Err(OwnedFileTreeError::FileDirectoryConflict { .. })
        ));

        let mut directory_first = OwnedFileTreeBuilder::new();
        directory_first
            .insert(projected_path("pages/index.ts"), &b"child"[..])
            .unwrap();
        assert!(matches!(
            directory_first.insert(projected_path("pages"), &b"file"[..]),
            Err(OwnedFileTreeError::FileDirectoryConflict { .. })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn owned_tree_rejects_case_equivalent_file_and_directory_identities() {
        let mut exact = OwnedFileTreeBuilder::new();
        exact
            .insert(projected_path("Pages/Entry.ts"), &b"one"[..])
            .unwrap();
        assert!(matches!(
            exact.insert(projected_path("pages/entry.ts"), &b"two"[..]),
            Err(OwnedFileTreeError::CaseEquivalentPath { .. })
        ));

        let mut shared_directory = OwnedFileTreeBuilder::new();
        shared_directory
            .insert(projected_path("Pages/a.ts"), &b"a"[..])
            .unwrap();
        assert!(matches!(
            shared_directory.insert(projected_path("pages/b.ts"), &b"b"[..]),
            Err(OwnedFileTreeError::CaseEquivalentPath { .. })
        ));

        let mut file_directory = OwnedFileTreeBuilder::new();
        file_directory
            .insert(projected_path("PAGES"), &b"file"[..])
            .unwrap();
        assert!(matches!(
            file_directory.insert(projected_path("pages/index.ts"), &b"child"[..]),
            Err(OwnedFileTreeError::FileDirectoryConflict { .. })
        ));
    }

    #[test]
    fn sealed_tree_owns_bytes_and_has_a_stable_inventory() {
        let mut builder = OwnedFileTreeBuilder::new();
        let mut source = b"original".to_vec();
        let shared: Arc<[u8]> = Arc::from(&b"shared"[..]);
        builder
            .insert(projected_path("z-last.ts"), source.clone())
            .unwrap();
        builder
            .insert(projected_path("a-first.ts"), &b"first"[..])
            .unwrap();
        builder
            .insert(projected_path("m-shared.ts"), Arc::clone(&shared))
            .unwrap();
        source.fill(b'x');

        let tree = builder.seal();
        assert_eq!(tree.len(), 3);
        assert!(!tree.is_empty());
        assert_eq!(
            tree.get(&projected_path("z-last.ts")),
            Some(&b"original"[..])
        );
        assert!(Arc::ptr_eq(
            tree.get_shared(&projected_path("m-shared.ts")).unwrap(),
            &shared
        ));
        assert_eq!(
            tree.inventory()
                .map(|path| path.as_path().to_path_buf())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("a-first.ts"),
                PathBuf::from("m-shared.ts"),
                PathBuf::from("z-last.ts")
            ]
        );
        assert_eq!(
            tree.iter()
                .map(|(path, contents)| (path.as_path().to_path_buf(), contents.as_ref().to_vec()))
                .collect::<Vec<_>>(),
            vec![
                (PathBuf::from("a-first.ts"), b"first".to_vec()),
                (PathBuf::from("m-shared.ts"), b"shared".to_vec()),
                (PathBuf::from("z-last.ts"), b"original".to_vec()),
            ]
        );
    }

    #[test]
    fn owned_root_operations_never_touch_the_base_filesystem() {
        let logical_root = PathBuf::from("project/.wake");
        let base: Arc<dyn FileSystem> = Arc::new(OwnedRootPanicFileSystem {
            forbidden_root: logical_root.clone(),
        });
        let fs = OwnedOverlayFileSystem::try_new(
            base,
            &logical_root,
            owned_tree(&[("nested/entry.txt", "owned")]),
        )
        .unwrap();
        let entry = logical_root.join("nested/entry.txt");
        let nested = logical_root.join("nested");
        let rogue = logical_root.join("rogue.txt");

        assert_eq!(fs.canonicalize(&entry).unwrap(), entry);
        assert_eq!(fs.canonicalize(&nested).unwrap(), nested);
        assert_eq!(fs.read_to_string(&entry).unwrap(), "owned");
        assert_eq!(fs.read(&entry).unwrap(), b"owned");
        assert!(fs.exists(&entry));
        assert!(fs.is_file(&entry));
        assert!(fs.is_dir(&nested));
        assert_eq!(fs.read_dir(&nested).unwrap(), vec![entry.clone()]);

        assert_eq!(
            fs.read_to_string(&rogue).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(fs.read(&rogue).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!fs.exists(&rogue));
        assert!(!fs.is_file(&rogue));
        assert!(!fs.is_dir(&rogue));
        assert_eq!(
            fs.canonicalize(&rogue).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            fs.read_dir(&rogue).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            fs.read(&nested).unwrap_err().kind(),
            io::ErrorKind::IsADirectory
        );
        assert_eq!(
            fs.read_dir(&entry).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert_eq!(
            fs.read(&entry.join("child")).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
    }

    #[test]
    fn owned_overlay_derives_directories_and_merges_ancestor_listings() {
        let base: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::from_files([
            ("project/.wake/rogue.bin", "rogue"),
            ("project/src/index.ts", "source"),
        ]));
        let fs = OwnedOverlayFileSystem::try_new(
            base,
            "project/.wake",
            owned_tree(&[
                ("docs/generated/index.ts", "index"),
                ("manifest.json", "manifest"),
            ]),
        )
        .unwrap();

        assert_eq!(
            fs.read_dir(Path::new("project")).unwrap(),
            vec![PathBuf::from("project/.wake"), PathBuf::from("project/src")]
        );
        assert_eq!(
            fs.read_dir(Path::new("project/.wake")).unwrap(),
            vec![
                PathBuf::from("project/.wake/docs"),
                PathBuf::from("project/.wake/manifest.json"),
            ]
        );
        assert_eq!(
            fs.read_dir(Path::new("project/.wake/docs")).unwrap(),
            vec![PathBuf::from("project/.wake/docs/generated")]
        );
        assert!(!fs.exists(Path::new("project/.wake/rogue.bin")));

        let no_base: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::new());
        let nested = OwnedOverlayFileSystem::try_new(
            no_base,
            "workspace/project/.wake",
            owned_tree(&[("entry.ts", "entry")]),
        )
        .unwrap();
        assert_eq!(
            nested.read_dir(Path::new("")).unwrap(),
            vec![PathBuf::from("workspace")]
        );
        assert_eq!(
            nested.read_dir(Path::new("workspace")).unwrap(),
            vec![PathBuf::from("workspace/project")]
        );
        assert!(nested.exists(Path::new("workspace/project")));
        assert!(nested.is_dir(Path::new("workspace/project")));
    }

    #[test]
    fn empty_owned_tree_still_hides_a_physical_root() {
        let base: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::from_files([
            ("project/.wake/rogue.bin", "rogue"),
            ("project/src/index.ts", "source"),
        ]));
        let tree = OwnedFileTreeBuilder::new().seal();
        let fs = OwnedOverlayFileSystem::try_new(base, "project/.wake", tree).unwrap();

        assert_eq!(
            fs.read_dir(Path::new("project")).unwrap(),
            vec![PathBuf::from("project/src")]
        );
        assert!(!fs.exists(Path::new("project/.wake")));
        assert!(!fs.exists(Path::new("project/.wake/rogue.bin")));
        assert_eq!(
            fs.read_dir(Path::new("project/.wake")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn physical_root_mutation_cannot_change_sealed_overlay_bytes() {
        let temp = TestDirectory::new("host-mutation");
        let logical_root = temp.path().join("project/.wake");
        std::fs::create_dir_all(&logical_root).unwrap();
        std::fs::write(logical_root.join("entry.txt"), "host-before").unwrap();

        let fs = OwnedOverlayFileSystem::try_new(
            Arc::new(OsFileSystem),
            &logical_root,
            owned_tree(&[("entry.txt", "sealed")]),
        )
        .unwrap();
        std::fs::write(logical_root.join("entry.txt"), "host-after").unwrap();
        std::fs::write(logical_root.join("rogue.txt"), "rogue").unwrap();

        assert_eq!(
            fs.read_to_string(&logical_root.join("entry.txt")).unwrap(),
            "sealed"
        );
        assert!(!fs.exists(&logical_root.join("rogue.txt")));
        assert_eq!(
            fs.read_dir(&logical_root).unwrap(),
            vec![logical_root.join("entry.txt")]
        );
        assert_eq!(
            fs.read_dir(Path::new("")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        assert!(!fs.exists(Path::new("")));
        assert!(!fs.is_dir(Path::new("")));
    }

    #[test]
    fn overlay_clones_share_one_stable_sealed_tree() {
        let base = Arc::new(MemoryFileSystem::new());
        let fs = OwnedOverlayFileSystem::try_new(
            base.clone(),
            "project/.wake",
            owned_tree(&[("entry.txt", "sealed")]),
        )
        .unwrap();
        let cloned = fs.clone();
        base.insert("project/.wake/entry.txt", "host-change");
        base.insert("project/.wake/rogue.txt", "rogue");

        for filesystem in [fs, cloned] {
            assert_eq!(
                filesystem
                    .read_to_string(Path::new("project/.wake/entry.txt"))
                    .unwrap(),
                "sealed"
            );
            assert!(!filesystem.exists(Path::new("project/.wake/rogue.txt")));
        }
    }

    #[test]
    fn owned_overlay_preserves_each_base_method_and_original_outside_path() {
        let fs = OwnedOverlayFileSystem::try_new(
            Arc::new(ExactDelegationFileSystem),
            "project/.wake",
            OwnedFileTreeBuilder::new().seal(),
        )
        .unwrap();
        let outside = Path::new("outside/./entry");

        assert_eq!(
            fs.canonicalize(outside).unwrap(),
            PathBuf::from("base-canonical")
        );
        assert_eq!(fs.read_to_string(outside).unwrap(), "base-string-method");
        assert_eq!(fs.read(outside).unwrap(), b"base-read-method");
        assert!(fs.exists(outside));
        assert!(fs.is_file(outside));
        assert!(fs.is_dir(outside));
        assert_eq!(
            fs.read_dir(outside).unwrap(),
            vec![PathBuf::from("base-listing")]
        );
    }

    #[test]
    fn owned_overlay_rejects_ambiguous_logical_roots() {
        let base: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::new());
        let tree = OwnedFileTreeBuilder::new().seal();
        for root in ["", ".", "./.wake", "project/../.wake"] {
            assert!(matches!(
                OwnedOverlayFileSystem::try_new(Arc::clone(&base), root, tree.clone()),
                Err(OwnedFileTreeError::InvalidLogicalRoot { .. })
            ));
        }

        #[cfg(windows)]
        for root in [r"C:project\.wake", r"\project\.wake"] {
            assert!(matches!(
                OwnedOverlayFileSystem::try_new(Arc::clone(&base), root, tree.clone()),
                Err(OwnedFileTreeError::InvalidLogicalRoot { .. })
            ));
        }
    }

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
            vec![
                PathBuf::from("pkg/a.js"),
                PathBuf::from("pkg/b.js"),
                PathBuf::from("pkg/sub"),
            ]
        );
    }

    #[test]
    fn memory_fs_read_dir_distinguishes_files_and_missing_directories() {
        let fs = MemoryFileSystem::from_files([("pkg/file.js", "file")]);

        assert_eq!(
            fs.read_dir(Path::new("pkg/file.js")).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert_eq!(
            fs.read_dir(Path::new("missing")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn memory_fs_synthesizes_a_directory_from_nested_files_only() {
        let fs = std::sync::Arc::new(MemoryFileSystem::from_files([(
            "pkg/nested/file.js",
            "file",
        )]));
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = {
            let fs = std::sync::Arc::clone(&fs);
            std::thread::spawn(move || {
                sender.send(fs.read_dir(Path::new("pkg"))).unwrap();
            })
        };

        assert_eq!(
            receiver
                .recv_timeout(std::time::Duration::from_secs(2))
                .expect("MemoryFileSystem::read_dir must not re-enter its files mutex")
                .unwrap(),
            vec![PathBuf::from("pkg/nested")]
        );
        worker.join().unwrap();
    }

    #[test]
    fn memory_fs_directory_matching_is_component_aware() {
        let fs = MemoryFileSystem::from_files([
            ("pkg/nested/file.js", "package"),
            ("pkg-other/file.js", "sibling"),
        ]);

        assert_eq!(
            fs.read_dir(Path::new("pkg")).unwrap(),
            vec![PathBuf::from("pkg/nested")]
        );
        assert!(!fs.is_dir(Path::new("pk")));
        assert_eq!(
            fs.read_dir(Path::new("pk")).unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
    }

    #[test]
    fn projected_fs_keeps_physical_generation_paths_out_of_identity() {
        let inner: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::from_files([
            ("physical/generation/pages/a.tsx", "export default 1"),
            ("project/src/index.ts", "source"),
        ]));
        let fs = ProjectedFileSystem::try_new(
            inner,
            [FileSystemProjection::tree(
                "project/.wake/docs/generated",
                "physical/generation",
            )],
        )
        .unwrap();

        assert_eq!(
            fs.read_to_string(Path::new("project/.wake/docs/generated/pages/a.tsx"))
                .unwrap(),
            "export default 1"
        );
        assert_eq!(
            fs.canonicalize(Path::new("project/.wake/docs/generated/pages/a.tsx"))
                .unwrap(),
            PathBuf::from("project/.wake/docs/generated/pages/a.tsx")
        );
        assert_eq!(
            fs.canonicalize(Path::new("project/.wake/docs/generated/pages/missing.tsx"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            fs.read_dir(Path::new("project/.wake/docs/generated/pages"))
                .unwrap(),
            vec![PathBuf::from("project/.wake/docs/generated/pages/a.tsx")]
        );
        assert!(
            fs.read_dir(Path::new("project/.wake/docs/generated/pages"))
                .unwrap()
                .iter()
                .all(|path| !path.starts_with("physical"))
        );
    }

    #[test]
    fn projected_fs_hides_reserved_tree_but_synthesizes_allowed_children() {
        let inner: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::from_files([
            ("project/.wake/cache.bin", "cache"),
            ("project/.wake/dev-candidates/other/entry.tsx", "leak"),
            ("project/.wake/docs/generated/entry.tsx", "docs"),
            ("project/src/index.ts", "source"),
        ]));
        let fs = ProjectedFileSystem::try_new(
            inner,
            [FileSystemProjection::tree(
                "project/.wake/docs/generated",
                "project/.wake/docs/generated",
            )],
        )
        .unwrap()
        .with_hidden_tree("project/.wake");

        assert!(fs.is_dir(Path::new("project/.wake")));
        assert_eq!(
            fs.read_dir(Path::new("project/.wake")).unwrap(),
            vec![PathBuf::from("project/.wake/docs")]
        );
        assert!(!fs.exists(Path::new("project/.wake/cache.bin")));
        assert!(!fs.exists(Path::new("project/.wake/dev-candidates/other/entry.tsx")));
        assert_eq!(
            fs.read_to_string(Path::new("project/.wake/docs/generated/entry.tsx"))
                .unwrap(),
            "docs"
        );
    }

    #[test]
    fn projected_fs_rejects_overlapping_or_exact_prefix_mappings() {
        let inner: Arc<dyn FileSystem> = Arc::new(MemoryFileSystem::from_files([
            ("physical/entry.tsx", "entry"),
            ("physical/entry.tsx/child", "child"),
        ]));
        let exact = ProjectedFileSystem::try_new(
            Arc::clone(&inner),
            [FileSystemProjection::exact_file(
                "logical/entry.tsx",
                "physical/entry.tsx",
            )],
        )
        .unwrap();
        assert!(exact.is_file(Path::new("logical/entry.tsx")));
        assert!(!exact.is_file(Path::new("logical/entry.tsx/child")));
        assert!(
            ProjectedFileSystem::try_new(
                inner,
                [
                    FileSystemProjection::tree("logical", "physical/a"),
                    FileSystemProjection::tree("logical/nested", "physical/b"),
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn exact_file_projection_reads_an_os_file_without_a_directory_suffix() {
        let root = std::env::temp_dir().join(format!(
            "wake-projected-exact-file-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir(&root).unwrap();
        let physical = root.join("physical-entry.tsx");
        let logical = root.join("logical-entry.tsx");
        std::fs::write(&physical, "export default 1").unwrap();

        let projection = FileSystemProjection::exact_file(&logical, &physical);
        assert_eq!(projection.to_physical(&logical), physical);
        assert_eq!(projection.to_logical(&physical), logical);
        let fs = ProjectedFileSystem::try_new(Arc::new(OsFileSystem), [projection]).unwrap();
        assert!(fs.exists(&logical));
        assert!(fs.is_file(&logical));
        assert_eq!(fs.canonicalize(&logical).unwrap(), logical);
        assert_eq!(fs.read_to_string(&logical).unwrap(), "export default 1");

        let stable_generation = ProjectedFileSystem::try_new(
            Arc::new(OsFileSystem),
            [FileSystemProjection::exact_file(&physical, &physical)],
        )
        .unwrap()
        .with_hidden_tree(&root);
        assert_eq!(
            stable_generation.read_to_string(&physical).unwrap(),
            "export default 1"
        );

        std::fs::remove_file(&physical).unwrap();
        std::fs::remove_dir(&root).unwrap();
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
