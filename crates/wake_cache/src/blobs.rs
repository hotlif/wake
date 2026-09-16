//! Opaque content-addressed derived artifacts. Directory and publication ownership stay here;
//! callers own keys, payload semantics, and whether a decoded blob is suitable for reuse.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tempfile::NamedTempFile;
use xxhash_rust::xxh3::Xxh3;

use crate::CacheDecodeError;

const MAGIC: &[u8; 4] = b"WLB1";
const SCHEMA: u32 = 1;
const HEADER: usize = 64;
const MAX_VALUE: usize = 4 * 1024 * 1024;
const MAX_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;

#[derive(Debug)]
pub enum BlobLoadOutcome {
    Loaded(Vec<u8>),
    Missing,
    Incompatible { found_schema: u32 },
    Corrupt(CacheDecodeError),
    Io(io::Error),
}

#[derive(Debug, Default)]
pub struct BlobStoreReport {
    pub written: usize,
    pub unchanged: usize,
    pub conflicts: usize,
    pub repaired: usize,
    pub evicted: usize,
}

pub struct BlobCache {
    directory: PathBuf,
}

impl BlobCache {
    /// Compute a cache location without creating it. The directory must be absolute; reads never
    /// create directories, and storage rejects symlink components and non-directory ancestors.
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn load(&self, key: &[u8; 32]) -> BlobLoadOutcome {
        match directories(&self.directory, false) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return BlobLoadOutcome::Missing;
            }
            Err(error) => return BlobLoadOutcome::Io(error),
        }
        let path = self.entry_path(key);
        let file = match open_regular(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return BlobLoadOutcome::Missing;
            }
            Err(error) => return BlobLoadOutcome::Io(error),
        };
        decode(file, key)
    }

    /// Store only values authored by this operation. Each entry is atomic; a batch is not one
    /// transaction. Equal facts coalesce, conflicting immutable facts are discarded under lock.
    pub fn store(&self, values: &[([u8; 32], Vec<u8>)]) -> io::Result<BlobStoreReport> {
        self.store_inner(values, MAX_ENTRIES, MAX_BYTES, || Ok(()))
    }

    fn store_inner(
        &self,
        values: &[([u8; 32], Vec<u8>)],
        max_entries: usize,
        max_bytes: u64,
        mut before_replace: impl FnMut() -> io::Result<()>,
    ) -> io::Result<BlobStoreReport> {
        let mut authored = BTreeMap::new();
        let mut total = 0usize;
        for (key, value) in values {
            if value.len() > MAX_VALUE {
                return Err(invalid("blob exceeds 4 MiB"));
            }
            total = total
                .checked_add(value.len() + HEADER)
                .ok_or_else(|| invalid("blob batch size overflow"))?;
            if total as u64 > MAX_BYTES || values.len() > MAX_ENTRIES {
                return Err(invalid("blob batch exceeds storage budget"));
            }
            if let Some(previous) = authored.insert(key, value)
                && previous != value
            {
                return Err(invalid("conflicting keys within blob batch"));
            }
        }
        if authored.is_empty() {
            return Ok(BlobStoreReport::default());
        }
        directories(&self.directory, true)?;
        let lock_path = self.directory.join(".write.lock");
        reject_special(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        crate::acquire_lock(&lock, Duration::from_millis(500))?;
        let mut report = BlobStoreReport::default();
        for (key, value) in authored {
            let destination = self.entry_path(key);
            match self.load(key) {
                BlobLoadOutcome::Loaded(previous) if previous == *value => {
                    report.unchanged += 1;
                    continue;
                }
                BlobLoadOutcome::Loaded(_) => {
                    std::fs::remove_file(&destination)?;
                    report.conflicts += 1;
                    continue;
                }
                BlobLoadOutcome::Corrupt(_) => report.repaired += 1,
                BlobLoadOutcome::Io(error) => return Err(error),
                BlobLoadOutcome::Missing | BlobLoadOutcome::Incompatible { .. } => {}
            }
            let mut header = [0u8; HEADER];
            header[..4].copy_from_slice(MAGIC);
            header[4..8].copy_from_slice(&SCHEMA.to_le_bytes());
            header[8..40].copy_from_slice(key);
            header[40..48].copy_from_slice(&(value.len() as u64).to_le_bytes());
            let digest = checksum(&header[..48], value);
            header[48..64].copy_from_slice(&digest.to_le_bytes());
            let mut temporary = NamedTempFile::new_in(&self.directory)?;
            temporary.write_all(&header)?;
            temporary.write_all(value)?;
            temporary.flush()?;
            temporary.as_file().sync_all()?;
            before_replace()?;
            reject_special(&destination)?;
            temporary
                .persist(&destination)
                .map_err(|error| error.error)?;
            report.written += 1;
        }
        report.evicted = self.compact(max_entries, max_bytes)?;
        Ok(report)
    }

    fn entry_path(&self, key: &[u8; 32]) -> PathBuf {
        let mut name = String::with_capacity(68);
        for byte in key {
            use std::fmt::Write;
            write!(name, "{byte:02x}").expect("String write");
        }
        name.push_str(".wlc");
        self.directory.join(name)
    }

    fn compact(&self, max_entries: usize, max_bytes: u64) -> io::Result<usize> {
        let mut entries = BinaryHeap::new();
        let mut bytes = 0u64;
        let mut removed = 0;
        for entry in std::fs::read_dir(&self.directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !owned_name(name) {
                continue;
            }
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                continue;
            }
            bytes = bytes.saturating_add(metadata.len());
            entries.push(Reverse((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                name.to_owned(),
                metadata.len(),
            )));
            while entries.len() > max_entries || bytes > max_bytes {
                let Reverse((_, name, length)) =
                    entries.pop().expect("over-budget retention has an entry");
                // Only validated content-key names inside this directory are candidates. Keeping
                // a bounded heap prevents an oversized preexisting directory exhausting memory.
                std::fs::remove_file(self.directory.join(name))?;
                bytes = bytes.saturating_sub(length);
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn owned_name(name: &str) -> bool {
    name.len() == 68
        && name.ends_with(".wlc")
        && name.as_bytes()[..64]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn reject_special(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(invalid("cache entry is not a regular file"))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_regular(path: &Path) -> io::Result<File> {
    reject_special(path)?;
    File::open(path)
}

fn directories(path: &Path, create: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(invalid("cache directory must be absolute"));
    }
    let ancestors: Vec<_> = path.ancestors().collect();
    for directory in ancestors.into_iter().rev() {
        let metadata = match std::fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && create => {
                match std::fs::create_dir(directory) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => return Err(error),
                }
                std::fs::symlink_metadata(directory)?
            }
            Err(error) => return Err(error),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid(
                "cache directory contains a symlink or non-directory component",
            ));
        }
    }
    Ok(())
}

fn checksum(prefix: &[u8], payload: &[u8]) -> u128 {
    let mut hash = Xxh3::new();
    hash.update(prefix);
    hash.update(payload);
    hash.digest128()
}

fn decode(mut file: File, key: &[u8; 32]) -> BlobLoadOutcome {
    let mut header = [0u8; HEADER];
    if let Err(error) = file.read_exact(&mut header) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            BlobLoadOutcome::Corrupt(CacheDecodeError::TruncatedHeader)
        } else {
            BlobLoadOutcome::Io(error)
        };
    }
    if &header[..4] != MAGIC {
        return BlobLoadOutcome::Corrupt(CacheDecodeError::InvalidMagic);
    }
    let schema = u32::from_le_bytes(header[4..8].try_into().unwrap());
    if schema != SCHEMA {
        return BlobLoadOutcome::Incompatible {
            found_schema: schema,
        };
    }
    if &header[8..40] != key {
        return BlobLoadOutcome::Corrupt(CacheDecodeError::InvalidValue("blob key"));
    }
    let length = u64::from_le_bytes(header[40..48].try_into().unwrap());
    if length > MAX_VALUE as u64 {
        return BlobLoadOutcome::Corrupt(CacheDecodeError::PayloadTooLarge {
            declared: length,
            maximum: MAX_VALUE,
        });
    }
    let expected = u128::from_le_bytes(header[48..64].try_into().unwrap());
    let mut payload = Vec::new();
    if payload.try_reserve_exact(length as usize).is_err() {
        return BlobLoadOutcome::Corrupt(CacheDecodeError::AllocationFailed("blob payload"));
    }
    payload.resize(length as usize, 0);
    if let Err(error) = file.read_exact(&mut payload) {
        return if error.kind() == io::ErrorKind::UnexpectedEof {
            BlobLoadOutcome::Corrupt(CacheDecodeError::TruncatedPayload)
        } else {
            BlobLoadOutcome::Io(error)
        };
    }
    let mut trailing = [0u8; 1];
    match file.read(&mut trailing) {
        Ok(0) => {}
        Ok(_) => return BlobLoadOutcome::Corrupt(CacheDecodeError::TrailingBytes),
        Err(error) => return BlobLoadOutcome::Io(error),
    }
    if checksum(&header[..48], &payload) != expected {
        return BlobLoadOutcome::Corrupt(CacheDecodeError::ChecksumMismatch);
    }
    BlobLoadOutcome::Loaded(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_is_bounded_and_keeps_unowned_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_owned());
        let unowned = dir.path().join("notes.txt");
        std::fs::write(&unowned, "keep").unwrap();
        let report = cache
            .store_inner(
                &[
                    ([1; 32], vec![1; 80]),
                    ([2; 32], vec![2; 80]),
                    ([3; 32], vec![3; 80]),
                ],
                2,
                200,
                || Ok(()),
            )
            .unwrap();
        assert_eq!(report.evicted, 2);
        assert_eq!(std::fs::read_to_string(unowned).unwrap(), "keep");
    }

    #[test]
    fn failed_replacement_preserves_previous_corrupt_entry_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_owned());
        let path = cache.entry_path(&[1; 32]);
        std::fs::write(&path, "old corrupt bytes").unwrap();
        let result = cache.store_inner(
            &[([1; 32], b"new".to_vec())],
            MAX_ENTRIES,
            MAX_BYTES,
            || Err(io::Error::other("injected replacement failure")),
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old corrupt bytes");
        assert_eq!(
            cache.store(&[([1; 32], b"new".to_vec())]).unwrap().repaired,
            1
        );
    }
}
