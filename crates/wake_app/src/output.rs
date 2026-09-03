use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Serialize, Serializer};
use wake_common::FileSystem;

use super::WakeError;

/// One exact-file publication.
pub(super) struct ExactOutput<'a> {
    path: &'a Path,
    bytes: &'a [u8],
}

impl<'a> ExactOutput<'a> {
    pub(super) fn write(path: &'a Path, bytes: &'a [u8]) -> Self {
        Self { path, bytes }
    }
}

/// Records successful content reads while preserving the exact `FileSystem` behavior used by the
/// bundler. Resolution probes are deliberately not recorded: only files whose bytes influenced the
/// candidate output belong to the protected input set.
#[derive(Clone)]
pub(super) struct RecordingFileSystem {
    inner: Arc<dyn FileSystem>,
    inputs: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl RecordingFileSystem {
    pub(super) fn new(inner: Arc<dyn FileSystem>) -> Self {
        Self {
            inner,
            inputs: Arc::new(Mutex::new(BTreeSet::new())),
        }
    }

    pub(super) fn inputs(&self) -> Vec<PathBuf> {
        self.inputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .cloned()
            .collect()
    }

    fn record(&self, path: &Path) {
        self.inputs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(path.to_path_buf());
    }
}

impl FileSystem for RecordingFileSystem {
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.canonicalize(path)
    }

    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        let source = self.inner.read_to_string(path)?;
        self.record(path);
        Ok(source)
    }

    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        let bytes = self.inner.read(path)?;
        self.record(path);
        Ok(bytes)
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

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
}

static OUTPUT_COMMIT: Mutex<()> = Mutex::new(());
pub(super) const OUTPUT_COMMIT_LOCK_FILE: &str = ".wake-output.lock";
pub(super) const OUTPUT_COMMIT_LOCK_NAMESPACE: &str = "wake-output-publication-v1";
const OUTPUT_COMMIT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);
const OUTPUT_COMMIT_LOCK_RETRY: Duration = Duration::from_millis(10);

#[cfg(unix)]
const OUTPUT_COMMIT_LOCK_PATH: &str = "/tmp/wake-output-publication-v1.lock";

#[cfg(windows)]
const OUTPUT_COMMIT_MUTEX_NAME: &str = "Global\\wake-output-publication-v1";

/// The commit guard shared by every overlapping directory and exact-file publication.
///
/// The process mutex makes thread semantics independent from operating-system lock quirks. The OS
/// lock has one environment-independent name for the whole machine/OS namespace, so nested and
/// cross-project output scopes cannot select different lock inodes. Unix uses one fixed `/tmp`
/// advisory-lock file; Windows uses a named mutex and treats an abandoned owner as acquisition.
/// Exact outputs may never target the retained `.wake-output.lock` migration namespace, and Unix
/// output scopes are revalidated against the live global lock inode.
pub(super) struct OutputCommitLock {
    _process: MutexGuard<'static, ()>,
    _os: OsOutputCommitLock,
    lock_paths: Vec<PathBuf>,
}

impl OutputCommitLock {
    pub(super) fn lock_paths(&self) -> &[PathBuf] {
        &self.lock_paths
    }
}

#[cfg(unix)]
struct OsOutputCommitLock {
    _file: File,
}

#[cfg(windows)]
struct OsOutputCommitLock {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for OsOutputCommitLock {
    fn drop(&mut self) {
        // SAFETY: `handle` is a live mutex handle returned by `CreateMutexW`, and this guard is
        // constructed only after the current thread acquired it. Drop runs exactly once.
        unsafe {
            windows_sys::Win32::System::Threading::ReleaseMutex(self.handle);
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(unix)]
fn open_unix_output_commit_lock(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

    loop {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "global output commit lock is not a regular file",
                    ));
                }
                let mut read_write = OpenOptions::new();
                read_write
                    .read(true)
                    .write(true)
                    .custom_flags(libc::O_NOFOLLOW);
                match read_write.open(path) {
                    Ok(file) => return Ok(file),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(_) => {
                        let mut read_only = OpenOptions::new();
                        read_only.read(true).custom_flags(libc::O_NOFOLLOW);
                        match read_only.open(path) {
                            Ok(file) => return Ok(file),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut create = OpenOptions::new();
                create
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .mode(0o666)
                    .custom_flags(libc::O_NOFOLLOW);
                match create.open(path) {
                    Ok(file) => return Ok(file),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn acquire_output_commit_lock(product: &str) -> Result<OutputCommitLock, WakeError> {
    let started = Instant::now();
    let process = loop {
        match OUTPUT_COMMIT.try_lock() {
            Ok(process) => break process,
            Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                let remaining = OUTPUT_COMMIT_LOCK_TIMEOUT.saturating_sub(started.elapsed());
                if remaining.is_zero() {
                    return Err(WakeError::new(
                        "WAKE_IO",
                        format!(
                            "timed out waiting for {product} output commit process gate `{OUTPUT_COMMIT_LOCK_NAMESPACE}`"
                        ),
                    ));
                }
                thread::sleep(remaining.min(OUTPUT_COMMIT_LOCK_RETRY));
            }
        }
    };

    #[cfg(unix)]
    {
        let path = PathBuf::from(OUTPUT_COMMIT_LOCK_PATH);
        let file = open_unix_output_commit_lock(&path).map_err(|error| {
            WakeError::new(
                "WAKE_IO",
                format!(
                    "{product} output commit lock failed while opening `{}`: {error}",
                    path.display()
                ),
            )
            .at(&path)
        })?;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(error) => {
                    let error: std::io::Error = error.into();
                    if error.kind() != std::io::ErrorKind::WouldBlock {
                        return Err(WakeError::new(
                            "WAKE_IO",
                            format!(
                                "{product} output commit lock failed for `{}`: {error}",
                                path.display()
                            ),
                        )
                        .at(&path));
                    }
                    let remaining = OUTPUT_COMMIT_LOCK_TIMEOUT.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(WakeError::new(
                            "WAKE_IO",
                            format!(
                                "timed out waiting for {product} output commit lock `{}`",
                                path.display()
                            ),
                        )
                        .at(&path));
                    }
                    thread::sleep(remaining.min(OUTPUT_COMMIT_LOCK_RETRY));
                }
            }
        }
        let opened = same_file::Handle::from_file(
            file.try_clone()
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?,
        )
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
        let named = same_file::Handle::from_path(&path)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(&path))?;
        if opened != named {
            return Err(WakeError::new(
                "WAKE_OUTPUT_COLLISION",
                "Wake's global output commit lock identity changed while acquiring it",
            )
            .at(&path));
        }
        Ok(OutputCommitLock {
            _process: process,
            _os: OsOutputCommitLock { _file: file },
            lock_paths: vec![path],
        })
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};

        let name = OUTPUT_COMMIT_MUTEX_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: the security-attributes pointer is null, ownership is initially false, and
        // `name` is a live NUL-terminated UTF-16 buffer for the duration of the call.
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
        if handle.is_null() {
            return Err(WakeError::new(
                "WAKE_IO",
                format!(
                    "{product} output commit mutex `{OUTPUT_COMMIT_LOCK_NAMESPACE}` could not be opened: {}",
                    std::io::Error::last_os_error()
                ),
            ));
        }
        let remaining = OUTPUT_COMMIT_LOCK_TIMEOUT.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            // SAFETY: the mutex was opened but never acquired.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
            return Err(WakeError::new(
                "WAKE_IO",
                format!(
                    "timed out waiting for {product} output commit mutex `{OUTPUT_COMMIT_LOCK_NAMESPACE}`"
                ),
            ));
        }
        let timeout_ms =
            u32::try_from(remaining.as_millis()).expect("output lock timeout fits in u32");
        // SAFETY: `handle` is a live mutex handle and the bounded timeout contains no pointers.
        let wait = unsafe { WaitForSingleObject(handle, timeout_ms) };
        match wait {
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(OutputCommitLock {
                _process: process,
                _os: OsOutputCommitLock { handle },
                lock_paths: Vec::new(),
            }),
            WAIT_TIMEOUT => {
                // SAFETY: the mutex was not acquired, but the live handle must still be closed.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                Err(WakeError::new(
                    "WAKE_IO",
                    format!(
                        "timed out waiting for {product} output commit mutex `{OUTPUT_COMMIT_LOCK_NAMESPACE}`"
                    ),
                ))
            }
            _ => {
                let error = std::io::Error::last_os_error();
                // SAFETY: the wait failed, but the live handle must still be closed.
                unsafe { windows_sys::Win32::Foundation::CloseHandle(handle) };
                Err(WakeError::new(
                    "WAKE_IO",
                    format!(
                        "{product} output commit mutex `{OUTPUT_COMMIT_LOCK_NAMESPACE}` failed: {error}"
                    ),
                ))
            }
        }
    }
}

#[derive(Clone, Debug)]
struct PathIdentity {
    lexical: String,
    physical: String,
    file: Option<Arc<same_file::Handle>>,
}

struct StagedOutput {
    path: PathBuf,
    staged: Option<tempfile::NamedTempFile>,
    backup: Option<PathBuf>,
    installed: bool,
}

/// Atomically publishes a complete exact-file set.
///
/// Every candidate is validated against every successfully read input before any parent directory
/// or temporary file is created. The global output lock is acquired before any same-directory
/// temporary is created; all byte payloads are then staged beside their destination and synced.
/// Existing files are moved to unique same-directory backups before any new file becomes visible;
/// every handled error rolls the whole set back before the lock is released.
pub(super) fn publish_exact_outputs(
    candidates: &[ExactOutput<'_>],
    protected_inputs: &[PathBuf],
) -> Result<(), WakeError> {
    publish_exact_outputs_inner(candidates, protected_inputs, None, || {})
}

fn publish_exact_outputs_inner(
    candidates: &[ExactOutput<'_>],
    protected_inputs: &[PathBuf],
    fail_after_installs: Option<usize>,
    after_staging: impl FnOnce(),
) -> Result<(), WakeError> {
    if candidates.is_empty() {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            "exact output transaction requires at least one candidate",
        ));
    }

    validate_exact_output_set(candidates, protected_inputs)?;
    validate_exact_output_lock_names(candidates)?;
    let commit = acquire_output_commit_lock("exact-file")?;
    // A protected input or destination can change while waiting. Detect that before creating even
    // destination-parent metadata, then keep the same global guard through all remaining work.
    validate_exact_output_set(candidates, protected_inputs)?;
    for candidate in candidates {
        let parent = candidate.path.parent().ok_or_else(|| {
            WakeError::new("WAKE_CONFIG", "exact output requires a parent directory")
                .at(candidate.path)
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent))?;
    }
    // The complete output/input set and the reserved lock identity are checked while every other
    // Wake publisher is excluded. The guard is deliberately acquired before a NamedTempFile is
    // created: an ancestor directory publisher must not move or delete exact staging in between.
    validate_exact_output_set(candidates, protected_inputs)?;
    validate_exact_output_commit_scope(candidates, commit.lock_paths())?;

    let mut staged = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let parent = candidate.path.parent().expect("validated parent");
        let mut temporary = tempfile::Builder::new()
            .prefix(".wake-exact-stage-")
            .tempfile_in(parent)
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent))?;
        temporary
            .write_all(candidate.bytes)
            .and_then(|()| temporary.flush())
            .and_then(|()| temporary.as_file().sync_all())
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(candidate.path))?;
        staged.push(StagedOutput {
            path: candidate.path.to_path_buf(),
            staged: Some(temporary),
            backup: None,
            installed: false,
        });
    }
    after_staging();
    // Staging can take time and external non-Wake actors are outside the commit lock protocol, so
    // repeat both identity checks immediately before the first destination mutation.
    validate_exact_output_set(candidates, protected_inputs)?;
    validate_exact_output_commit_scope(candidates, commit.lock_paths())?;

    // Deterministic ordering makes fault behavior and rollback tests independent from caller order.
    staged.sort_by_key(|item| path_key(&item.path));
    for index in 0..staged.len() {
        let path = staged[index].path.clone();
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.is_dir() {
                    let error = WakeError::new(
                        "WAKE_OUTPUT_COLLISION",
                        "exact output destination is a directory",
                    )
                    .at(&path);
                    rollback_exact_outputs(&mut staged, error)?;
                }
                let parent = path.parent().expect("validated parent");
                let placeholder = tempfile::Builder::new()
                    .prefix(".wake-exact-backup-")
                    .tempfile_in(parent)
                    .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(parent));
                let placeholder = match placeholder {
                    Ok(placeholder) => placeholder,
                    Err(error) => return rollback_exact_outputs(&mut staged, error),
                };
                let backup = placeholder.path().to_path_buf();
                if let Err(error) = placeholder.close() {
                    rollback_exact_outputs(
                        &mut staged,
                        WakeError::new("WAKE_IO", error.to_string()).at(parent),
                    )?;
                }
                if let Err(error) = std::fs::rename(&path, &backup) {
                    rollback_exact_outputs(
                        &mut staged,
                        WakeError::new("WAKE_IO", error.to_string()).at(&path),
                    )?;
                }
                staged[index].backup = Some(backup);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => rollback_exact_outputs(
                &mut staged,
                WakeError::new("WAKE_IO", error.to_string()).at(&path),
            )?,
        }
    }

    let mut installs = 0;
    for index in 0..staged.len() {
        if fail_after_installs == Some(installs) {
            let path = staged[index].path.clone();
            rollback_exact_outputs(
                &mut staged,
                WakeError::new("WAKE_IO", "injected exact output install failure").at(&path),
            )?;
        }
        let temporary = staged[index].staged.take().expect("checked above");
        let path = staged[index].path.clone();
        match temporary.persist(&path) {
            Ok(_) => {
                staged[index].installed = true;
                installs += 1;
            }
            Err(error) => {
                staged[index].staged = Some(error.file);
                rollback_exact_outputs(
                    &mut staged,
                    WakeError::new("WAKE_IO", error.error.to_string()).at(&path),
                )?;
            }
        }
    }

    // All desired files are now visible, which is the transaction commit point. Backup deletion is
    // post-commit garbage collection: reporting it as a publication failure would be dishonest
    // because a backup removed earlier in this loop can no longer participate in rollback.
    for item in &mut staged {
        if let Some(backup) = item.backup.take() {
            for attempt in 0..3 {
                match std::fs::remove_file(&backup) {
                    Ok(()) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                    Err(_) if attempt < 2 => std::thread::yield_now(),
                    // A uniquely named backup is safe to retain for later manual cleanup. The
                    // committed destination set remains complete and is never rolled back here.
                    Err(_) => break,
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn publish_exact_outputs_with_staging_hook(
    candidates: &[ExactOutput<'_>],
    protected_inputs: &[PathBuf],
    after_staging: impl FnOnce(),
) -> Result<(), WakeError> {
    publish_exact_outputs_inner(candidates, protected_inputs, None, after_staging)
}

fn rollback_exact_outputs(
    staged: &mut [StagedOutput],
    mut original: WakeError,
) -> Result<(), WakeError> {
    let mut rollback_errors = Vec::new();
    for item in staged.iter_mut().rev() {
        if item.installed {
            match std::fs::remove_file(&item.path) {
                Ok(()) => item.installed = false,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    item.installed = false;
                }
                Err(error) => rollback_errors.push(format!(
                    "could not remove partially installed `{}`: {error}",
                    item.path.display()
                )),
            }
        }
        if let Some(backup) = item.backup.take() {
            match std::fs::rename(&backup, &item.path) {
                Ok(()) => {}
                Err(error) => rollback_errors.push(format!(
                    "could not restore `{}` from `{}`: {error}",
                    item.path.display(),
                    backup.display()
                )),
            }
        }
    }
    if !rollback_errors.is_empty() {
        original
            .message
            .push_str("; exact output rollback was incomplete: ");
        original.message.push_str(&rollback_errors.join("; "));
    }
    Err(original)
}

fn validate_exact_output_set(
    candidates: &[ExactOutput<'_>],
    protected_inputs: &[PathBuf],
) -> Result<(), WakeError> {
    let inputs = protected_inputs
        .iter()
        .map(|path| identify_path(path, true))
        .collect::<Result<Vec<_>, _>>()?;
    let mut outputs = BTreeMap::<String, (&Path, PathIdentity)>::new();
    for candidate in candidates {
        if !candidate.path.is_absolute() || candidate.path.file_name().is_none() {
            return Err(WakeError::new(
                "WAKE_CONFIG",
                "exact output path must be an absolute file path",
            )
            .at(candidate.path));
        }
        let identity = identify_path(candidate.path, false)?;
        if let Some((previous, _)) = outputs.insert(
            identity.physical.clone(),
            (candidate.path, identity.clone()),
        ) {
            return Err(WakeError::new(
                "WAKE_OUTPUT_COLLISION",
                format!(
                    "exact outputs resolve to the same destination: {} and {}",
                    previous.display(),
                    candidate.path.display()
                ),
            )
            .at(candidate.path));
        }
        for input in &inputs {
            if identities_alias(&identity, input) {
                return Err(WakeError::new(
                    "WAKE_OUTPUT_COLLISION",
                    format!(
                        "refusing to publish exact output over a file read as build input: {}",
                        candidate.path.display()
                    ),
                )
                .at(candidate.path));
            }
        }
        for (_, previous) in outputs.values() {
            if previous.lexical != identity.lexical && identities_alias(&identity, previous) {
                return Err(WakeError::new(
                    "WAKE_OUTPUT_COLLISION",
                    "exact output paths are physical aliases of the same file",
                )
                .at(candidate.path));
            }
        }
    }
    Ok(())
}

fn validate_exact_output_lock_names(candidates: &[ExactOutput<'_>]) -> Result<(), WakeError> {
    for candidate in candidates {
        #[cfg(unix)]
        let is_global_lock =
            path_key(candidate.path) == path_key(Path::new(OUTPUT_COMMIT_LOCK_PATH));
        #[cfg(not(unix))]
        let is_global_lock = false;
        if is_output_commit_lock_path(candidate.path) || is_global_lock {
            return Err(WakeError::new(
                "WAKE_OUTPUT_COLLISION",
                "exact output uses Wake's reserved output-commit lock name",
            )
            .at(candidate.path));
        }
    }
    Ok(())
}

fn validate_exact_output_commit_scope(
    candidates: &[ExactOutput<'_>],
    lock_paths: &[PathBuf],
) -> Result<(), WakeError> {
    validate_exact_output_lock_names(candidates)?;
    let locks = lock_paths
        .iter()
        .map(|path| identify_path(path, true))
        .collect::<Result<Vec<_>, _>>()?;
    for candidate in candidates {
        let output = identify_path(candidate.path, false)?;
        if locks.iter().any(|lock| identities_alias(&output, lock)) {
            return Err(WakeError::new(
                "WAKE_OUTPUT_COLLISION",
                "exact output aliases a live Wake output-commit lock",
            )
            .at(candidate.path));
        }
    }
    Ok(())
}

pub(super) fn is_output_commit_lock_path(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        path_key(Path::new(name)) == path_key(Path::new(OUTPUT_COMMIT_LOCK_FILE))
    })
}

fn identities_alias(left: &PathIdentity, right: &PathIdentity) -> bool {
    left.lexical == right.lexical
        || left.physical == right.physical
        || left
            .file
            .as_ref()
            .zip(right.file.as_ref())
            .is_some_and(|(left, right)| left == right)
}

fn identify_path(path: &Path, must_exist: bool) -> Result<PathIdentity, WakeError> {
    let absolute = if path.is_absolute() {
        normalize_path(path)
    } else {
        let cwd = std::env::current_dir()
            .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(path))?;
        normalize_path(&cwd.join(path))
    };
    let lexical = path_key(&absolute);

    match std::fs::canonicalize(&absolute) {
        Ok(canonical) => {
            let canonical = normalize_path(&canonical);
            let file = same_file::Handle::from_path(&canonical)
                .map(Arc::new)
                .map_err(|error| WakeError::new("WAKE_IO", error.to_string()).at(path))?;
            Ok(PathIdentity {
                lexical,
                physical: path_key(&canonical),
                file: Some(file),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !must_exist => {
            let physical = project_missing_path(&absolute)?;
            Ok(PathIdentity {
                lexical,
                physical: path_key(&physical),
                file: None,
            })
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && looks_like_archive_virtual_path(&absolute) =>
        {
            Ok(PathIdentity {
                // Virtual archive paths can be valid reads without a standalone OS file. They
                // still participate in lexical comparisons, which is the only identity available.
                lexical: lexical.clone(),
                physical: lexical,
                file: None,
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(WakeError::new(
            "WAKE_OUTPUT_COLLISION",
            "a previously read input disappeared before exact output publication",
        )
        .at(path)),
        Err(error) => Err(WakeError::new("WAKE_IO", error.to_string()).at(path)),
    }
}

fn looks_like_archive_virtual_path(path: &Path) -> bool {
    path.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with(".zip")
    })
}

fn project_missing_path(path: &Path) -> Result<PathBuf, WakeError> {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    loop {
        match std::fs::canonicalize(&cursor) {
            Ok(existing) => {
                let mut physical = normalize_path(&existing);
                for component in missing.into_iter().rev() {
                    physical.push(component);
                }
                return Ok(physical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name() else {
                    return Err(WakeError::new(
                        "WAKE_OUTPUT_COLLISION",
                        "could not establish a physical identity for exact output",
                    )
                    .at(path));
                };
                missing.push(name.to_os_string());
                cursor = cursor.parent().unwrap_or(Path::new("")).to_path_buf();
            }
            Err(error) => return Err(WakeError::new("WAKE_IO", error.to_string()).at(&cursor)),
        }
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

#[cfg(windows)]
fn path_key(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches("//?/")
        .to_lowercase()
}

#[cfg(not(windows))]
fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The complete set of files that Wake can report from a product build.
///
/// This enum is intentionally exhaustive. Shell adapters must handle every
/// variant explicitly so adding a new output kind cannot silently change a
/// public wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutputFileKind {
    Asset,
    Chunk,
    Css,
    Declaration,
    Entry,
    FederationBootstrap,
    FederationChunk,
    FederationEntry,
    FederationManifest,
    FederationShared,
    FederationTypes,
    Html,
    Manifest,
    SourceMap,
}

impl OutputFileKind {
    /// Returns the stable string used by Wake's serialized application API.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Asset => "asset",
            Self::Chunk => "chunk",
            Self::Css => "css",
            Self::Declaration => "declaration",
            Self::Entry => "entry",
            Self::FederationBootstrap => "federation-bootstrap",
            Self::FederationChunk => "federation-chunk",
            Self::FederationEntry => "federation-entry",
            Self::FederationManifest => "federation-manifest",
            Self::FederationShared => "federation-shared",
            Self::FederationTypes => "types",
            Self::Html => "html",
            Self::Manifest => "manifest",
            Self::SourceMap => "map",
        }
    }
}

impl Serialize for OutputFileKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputFile {
    pub path: String,
    pub kind: OutputFileKind,
    pub bytes: usize,
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use wake_common::{FileSystem, OsFileSystem};

    use super::{
        ExactOutput, OutputFileKind, RecordingFileSystem, publish_exact_outputs,
        publish_exact_outputs_inner,
    };

    #[cfg(unix)]
    use super::acquire_output_commit_lock;

    #[test]
    fn output_file_kinds_have_stable_wire_names() {
        let cases = [
            (OutputFileKind::Asset, "asset"),
            (OutputFileKind::Chunk, "chunk"),
            (OutputFileKind::Css, "css"),
            (OutputFileKind::Declaration, "declaration"),
            (OutputFileKind::Entry, "entry"),
            (OutputFileKind::FederationBootstrap, "federation-bootstrap"),
            (OutputFileKind::FederationChunk, "federation-chunk"),
            (OutputFileKind::FederationEntry, "federation-entry"),
            (OutputFileKind::FederationManifest, "federation-manifest"),
            (OutputFileKind::FederationShared, "federation-shared"),
            (OutputFileKind::FederationTypes, "types"),
            (OutputFileKind::Html, "html"),
            (OutputFileKind::Manifest, "manifest"),
            (OutputFileKind::SourceMap, "map"),
        ];

        for (kind, expected) in cases {
            assert_eq!(kind.as_str(), expected);
            assert_eq!(serde_json::to_value(kind).unwrap(), expected);
        }
    }

    #[test]
    fn exact_output_commit_replaces_the_complete_set() {
        let fixture = tempfile::tempdir().unwrap();
        let code = fixture.path().join("bundle.js");
        let map = fixture.path().join("bundle.js.map");
        let sentinel = fixture.path().join("sentinel.txt");
        std::fs::write(&code, "old-code").unwrap();
        std::fs::write(&map, "old-map").unwrap();
        std::fs::write(&sentinel, "outside").unwrap();

        publish_exact_outputs(
            &[
                ExactOutput::write(&code, b"new-code"),
                ExactOutput::write(&map, b"new-map"),
            ],
            &[],
        )
        .unwrap();

        assert_eq!(std::fs::read_to_string(code).unwrap(), "new-code");
        assert_eq!(std::fs::read_to_string(map).unwrap(), "new-map");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside");
        assert_no_transaction_files(fixture.path());
    }

    #[test]
    fn exact_output_install_failure_restores_every_old_file() {
        let fixture = tempfile::tempdir().unwrap();
        let code = fixture.path().join("bundle.js");
        let map = fixture.path().join("bundle.js.map");
        let sentinel = fixture.path().join("sentinel.txt");
        std::fs::write(&code, "old-code").unwrap();
        std::fs::write(&map, "old-map").unwrap();
        std::fs::write(&sentinel, "outside").unwrap();

        let error = publish_exact_outputs_inner(
            &[
                ExactOutput::write(&code, b"new-code"),
                ExactOutput::write(&map, b"new-map"),
            ],
            &[],
            Some(1),
            || {},
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_IO");
        assert_eq!(std::fs::read_to_string(code).unwrap(), "old-code");
        assert_eq!(std::fs::read_to_string(map).unwrap(), "old-map");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside");
        assert_no_transaction_files(fixture.path());
    }

    #[cfg(unix)]
    #[test]
    fn exact_output_cannot_replace_the_shared_commit_lock_inode() {
        let commit = acquire_output_commit_lock("exact lock collision test").unwrap();
        let lock_path = commit
            .lock_paths()
            .first()
            .expect("Unix uses one global advisory-lock file")
            .clone();
        let original = same_file::Handle::from_path(&lock_path).unwrap();
        drop(commit);

        let error =
            publish_exact_outputs(&[ExactOutput::write(&lock_path, b"replace")], &[]).unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(same_file::Handle::from_path(lock_path).unwrap(), original);
    }

    #[test]
    fn exact_output_rejects_the_reserved_migration_lock_name() {
        let fixture = tempfile::tempdir().unwrap();
        let lock_path = fixture.path().join(".wake-output.lock");
        std::fs::write(&lock_path, "reserved").unwrap();
        let original = same_file::Handle::from_path(&lock_path).unwrap();

        let error =
            publish_exact_outputs(&[ExactOutput::write(&lock_path, b"replace")], &[]).unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(same_file::Handle::from_path(lock_path).unwrap(), original);
    }

    #[test]
    fn exact_output_rejects_a_hard_link_to_a_read_input() {
        let fixture = tempfile::tempdir().unwrap();
        let input = fixture.path().join("input.js");
        let output = fixture.path().join("alias.js");
        std::fs::write(&input, "source").unwrap();
        std::fs::hard_link(&input, &output).unwrap();

        let error = publish_exact_outputs(
            &[ExactOutput::write(&output, b"generated")],
            std::slice::from_ref(&input),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(std::fs::read_to_string(&input).unwrap(), "source");
        assert_eq!(std::fs::read_to_string(&output).unwrap(), "source");
    }

    #[test]
    fn exact_output_rejects_a_symbolic_alias_to_a_read_input_when_supported() {
        let fixture = tempfile::tempdir().unwrap();
        let input = fixture.path().join("input.js");
        let output = fixture.path().join("alias.js");
        std::fs::write(&input, "source").unwrap();
        if create_file_symlink(&input, &output).is_err() {
            return;
        }

        let error = publish_exact_outputs(
            &[ExactOutput::write(&output, b"generated")],
            std::slice::from_ref(&input),
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_OUTPUT_COLLISION");
        assert_eq!(std::fs::read_to_string(input).unwrap(), "source");
    }

    #[cfg(windows)]
    #[test]
    fn exact_output_locked_second_destination_rolls_back_the_first() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let fixture = tempfile::tempdir().unwrap();
        let code = fixture.path().join("bundle.js");
        let map = fixture.path().join("bundle.js.map");
        let sentinel = fixture.path().join("sentinel.txt");
        std::fs::write(&code, "old-code").unwrap();
        std::fs::write(&map, "old-map").unwrap();
        std::fs::write(&sentinel, "outside").unwrap();
        let _locked = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&map)
            .unwrap();

        let error = publish_exact_outputs(
            &[
                ExactOutput::write(&code, b"new-code"),
                ExactOutput::write(&map, b"new-map"),
            ],
            &[],
        )
        .unwrap_err();

        assert_eq!(error.code, "WAKE_IO");
        assert_eq!(std::fs::read_to_string(code).unwrap(), "old-code");
        assert_eq!(std::fs::read_to_string(map).unwrap(), "old-map");
        assert_eq!(std::fs::read_to_string(sentinel).unwrap(), "outside");
    }

    #[test]
    fn recording_file_system_reports_only_successful_content_reads() {
        let fixture = tempfile::tempdir().unwrap();
        let first = fixture.path().join("first.js");
        let second = fixture.path().join("second.bin");
        std::fs::write(&first, "first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        let fs = RecordingFileSystem::new(Arc::new(OsFileSystem));

        fs.read_to_string(&first).unwrap();
        fs.read(&second).unwrap();
        assert!(fs.read(fixture.path().join("missing").as_path()).is_err());

        assert_eq!(fs.inputs(), vec![first, second]);
    }

    fn assert_no_transaction_files(directory: &Path) {
        let leftovers = std::fs::read_dir(directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with(".wake-exact-stage-") || name.starts_with(".wake-exact-backup-")
            })
            .collect::<Vec<_>>();
        assert!(leftovers.is_empty(), "transaction leftovers: {leftovers:?}");
    }

    #[cfg(unix)]
    fn create_file_symlink(input: &Path, output: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(input, output)
    }

    #[cfg(windows)]
    fn create_file_symlink(input: &Path, output: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(input, output)
    }
}
