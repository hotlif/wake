//! Project-local retention for Worker module snapshots. The file contents use the BuildCache codec.

use crate::files::{directories, invalid, open_regular};
use crate::{
    BuildCache, CacheLoadOutcome, CacheStoreError, CacheStoreReport, HEADER_LEN, LOCK_WAIT_TIMEOUT,
    load_open_file,
};
use std::collections::{BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::io;
use std::path::Path;

const MAX_BYTES: usize = 256 * 1024 * 1024;

impl BuildCache {
    /// Read a Worker snapshot. Its absolute path ends in a 32-digit lowercase hex key and `.bin`.
    pub fn load_worker(path: &Path) -> CacheLoadOutcome {
        let file = directory(path, false).and_then(|_| open_regular(path));
        load_open_file(file, MAX_BYTES - HEADER_LEN)
    }

    /// Merge and publish a Worker snapshot under the project directory's shared writer lock.
    /// All managed snapshots together occupy at most 256 MiB, including their envelopes.
    pub fn store_worker(&mut self, path: &Path) -> Result<CacheStoreReport, CacheStoreError> {
        self.store_inner(
            path,
            Some(DirectoryLimits::default()),
            LOCK_WAIT_TIMEOUT,
            || Ok(()),
        )
    }
}

fn managed(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_suffix(".bin"))
        .is_some_and(|key| {
            key.len() == 32
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

pub(super) fn directory(path: &Path, create: bool) -> io::Result<&Path> {
    if !path.file_name().is_some_and(managed) {
        return Err(invalid("invalid Worker snapshot filename"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| invalid("missing Worker cache directory"))?;
    directories(parent, create)?;
    Ok(parent)
}

#[derive(Clone, Copy)]
pub(super) struct DirectoryLimits {
    pub(super) max_bytes: usize,
    max_files: usize,
}

impl Default for DirectoryLimits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            max_files: 4096,
        }
    }
}

impl DirectoryLimits {
    /// Reserve space before atomic replacement. The prior destination remains intact on failure.
    /// The bounded set holds the newest files, even in an oversized preexisting directory.
    pub(super) fn make_room(self, path: &Path, incoming: usize) -> io::Result<()> {
        let directory = path.parent().expect("validated Worker snapshot path");
        let mut keep = BTreeSet::new();
        let mut needs_eviction = false;
        other_snapshots(path, |name, metadata| {
            keep.insert((metadata.modified()?, name, metadata.len()));
            if keep.len() >= self.max_files {
                keep.pop_first();
                needs_eviction = true;
            }
            Ok(())
        })?;
        let mut bytes = keep
            .iter()
            .map(|(_, _, len)| u128::from(*len))
            .sum::<u128>()
            + incoming as u128;
        while bytes > self.max_bytes as u128 {
            let (_, _, length) = keep.pop_first().expect("incoming snapshot fits the budget");
            bytes -= u128::from(length);
            needs_eviction = true;
        }
        if needs_eviction {
            let names: HashSet<_> = keep.into_iter().map(|(_, name, _)| name).collect();
            other_snapshots(path, |name, _| {
                if !names.contains(&name) {
                    fs::remove_file(directory.join(name))?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

fn other_snapshots(
    path: &Path,
    mut visit: impl FnMut(OsString, Metadata) -> io::Result<()>,
) -> io::Result<()> {
    for entry in fs::read_dir(path.parent().expect("validated Worker snapshot path"))? {
        let entry = entry?;
        let name = entry.file_name();
        if Some(name.as_os_str()) == path.file_name() || !managed(&name) {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.is_file() {
            visit(name, metadata)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::DirectoryLimits;
    use crate::{
        BuildCache, CacheDecodeError, CacheLoadOutcome, CacheStoreError, CacheStoreStage,
        HEADER_LEN, ModuleSummary,
    };
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Barrier};
    use std::time::{Duration, SystemTime};

    fn entry(directory: &Path, key: u128) -> PathBuf {
        directory.join(format!("{key:032x}.bin"))
    }

    fn authored(key: u64) -> BuildCache {
        let mut cache = BuildCache::new();
        cache.put_summary(key, ModuleSummary::default());
        cache
    }

    fn store_with(cache: &mut BuildCache, path: &Path, limits: DirectoryLimits) {
        cache
            .store_inner(path, Some(limits), Duration::from_secs(2), || Ok(()))
            .unwrap();
    }

    #[test]
    fn worker_budget_counts_envelopes_and_evicts_oldest_managed_files() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("worker-cache");
        let old = entry(&directory, 1);
        let recent = entry(&directory, 2);
        let new = entry(&directory, 3);
        assert!(matches!(
            BuildCache::load_worker(&old),
            CacheLoadOutcome::Missing
        ));
        assert!(!directory.exists());
        authored(1).store_worker(&old).unwrap();
        authored(2).store_worker(&recent).unwrap();
        let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(past)
            .unwrap();
        let unrelated = directory.join("notes.bin");
        fs::write(&unrelated, [42; 256]).unwrap();
        fs::create_dir(entry(&directory, 9)).unwrap();

        let mut writer = authored(3);
        let size = fs::metadata(&old).unwrap().len() as usize;
        let limits = DirectoryLimits {
            max_bytes: 3 * size - 1,
            max_files: 3,
        };
        store_with(&mut writer, &new, limits);
        assert!(!old.exists());
        assert!(matches!(
            BuildCache::load_worker(&recent),
            CacheLoadOutcome::Loaded(_)
        ));
        assert!(matches!(
            BuildCache::load_worker(&new),
            CacheLoadOutcome::Loaded(_)
        ));
        assert_eq!(fs::read(unrelated).unwrap(), [42; 256]);
        assert!(entry(&directory, 9).is_dir());

        // Replacing an existing destination accounts for its new size exactly once.
        let replacement_length = [
            fs::metadata(&recent).unwrap().len(),
            fs::metadata(&new).unwrap().len(),
        ]
        .iter()
        .sum::<u64>() as usize;
        store_with(
            &mut authored(3),
            &new,
            DirectoryLimits {
                max_bytes: replacement_length,
                max_files: 2,
            },
        );
        assert!(recent.exists());
    }

    #[test]
    fn worker_sibling_writers_share_one_lock_and_directory_count_limit() {
        let temp = tempfile::tempdir().unwrap();
        let barrier = Arc::new(Barrier::new(5));
        std::thread::scope(|scope| {
            for key in 0..5 {
                let path = entry(temp.path(), key);
                let barrier = Arc::clone(&barrier);
                scope.spawn(move || {
                    barrier.wait();
                    store_with(
                        &mut authored(key as u64),
                        &path,
                        DirectoryLimits {
                            max_bytes: 4096,
                            max_files: 2,
                        },
                    );
                });
            }
        });
        let mut snapshots = 0;
        for item in fs::read_dir(temp.path()).unwrap() {
            let path = item.unwrap().path();
            if path.file_name().unwrap() == ".write.lock" {
                continue;
            }
            assert!(matches!(
                BuildCache::load_worker(&path),
                CacheLoadOutcome::Loaded(_)
            ));
            snapshots += 1;
        }
        assert_eq!(snapshots, 2);
        let lock = File::options()
            .read(true)
            .write(true)
            .open(temp.path().join(".write.lock"))
            .unwrap();
        lock.lock().unwrap();
        let mut writer = authored(99);
        let result = writer.store_inner(
            &entry(temp.path(), 99),
            Some(DirectoryLimits::default()),
            Duration::ZERO,
            || Ok(()),
        );
        assert!(matches!(
            result,
            Err(CacheStoreError::Io {
                stage: CacheStoreStage::Lock,
                ..
            })
        ));
        assert!(writer.is_dirty());
    }

    #[test]
    fn worker_failed_publication_preserves_destination_and_dirty_writer() {
        let temp = tempfile::tempdir().unwrap();
        let path = entry(temp.path(), 42);
        authored(1).store_worker(&path).unwrap();
        let original = fs::read(&path).unwrap();
        let mut writer = authored(2);
        let result = writer.store_inner(
            &path,
            Some(DirectoryLimits::default()),
            Duration::ZERO,
            || Err(io::Error::other("injected replace failure")),
        );
        assert!(matches!(
            result,
            Err(CacheStoreError::Io {
                stage: CacheStoreStage::Replace,
                ..
            })
        ));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert!(writer.is_dirty());
        writer.store_worker(&path).unwrap();
        let CacheLoadOutcome::Loaded(mut merged) = BuildCache::load_worker(&path) else {
            panic!("missing worker cache")
        };
        assert!(merged.summary(1).is_some());
        assert!(merged.summary(2).is_some());
    }

    #[test]
    fn worker_load_rejects_a_declared_payload_above_the_directory_limit_before_reading() {
        let temp = tempfile::tempdir().unwrap();
        let path = entry(temp.path(), 1);
        authored(1).store_worker(&path).unwrap();
        let mut header = fs::read(&path).unwrap()[..HEADER_LEN].to_vec();
        let declared = 256u64 * 1024 * 1024 - HEADER_LEN as u64 + 1;
        header[8..16].copy_from_slice(&declared.to_le_bytes());
        fs::write(&path, header).unwrap();
        assert!(matches!(
            BuildCache::load_worker(&path),
            CacheLoadOutcome::Corrupt(CacheDecodeError::PayloadTooLarge { .. })
        ));
        assert!(
            authored(2)
                .store_worker(&path)
                .unwrap()
                .repaired_corrupt_latest
        );
        assert!(matches!(
            BuildCache::load_worker(&path),
            CacheLoadOutcome::Loaded(_)
        ));
        let foreign = temp.path().join("unrelated.bin");
        fs::write(&foreign, b"original").unwrap();
        assert!(authored(1).store_worker(&foreign).is_err());
        assert_eq!(fs::read(foreign).unwrap(), b"original");
    }

    #[test]
    fn worker_store_reclaims_space_from_a_full_project_directory() {
        let temp = tempfile::tempdir().unwrap();
        let old = entry(temp.path(), 1);
        File::create(&old)
            .unwrap()
            .set_len(256 * 1024 * 1024)
            .unwrap();
        let new = entry(temp.path(), 2);
        authored(2).store_worker(&new).unwrap();
        assert!(!old.exists());
        assert!(matches!(
            BuildCache::load_worker(&new),
            CacheLoadOutcome::Loaded(_)
        ));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn worker_symlinks_are_excluded_from_load_store_and_eviction() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("worker-cache");
        fs::create_dir(&directory).unwrap();
        let outside = temp.path().join("outside.bin");
        fs::write(&outside, b"untouched").unwrap();
        let linked = entry(&directory, 9);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &linked).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_file(&outside, &linked) {
            if error.raw_os_error() == Some(1314) {
                eprintln!("Windows symlink privilege is unavailable");
                return;
            }
            panic!("create symlink: {error}");
        }
        assert!(matches!(
            BuildCache::load_worker(&linked),
            CacheLoadOutcome::Io(_)
        ));
        assert!(authored(9).store_worker(&linked).is_err());
        store_with(
            &mut authored(1),
            &entry(&directory, 1),
            DirectoryLimits {
                max_bytes: 4096,
                max_files: 1,
            },
        );
        assert!(linked.is_symlink());
        let alias = temp.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&directory, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&directory, &alias).unwrap();
        assert!(authored(3).store_worker(&entry(&alias, 3)).is_err());
        assert_eq!(fs::read(outside).unwrap(), b"untouched");
    }
}
