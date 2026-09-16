use wake_cache::{BlobCache, BlobLoadOutcome};

#[test]
fn blobs_round_trip_and_stale_writers_preserve_disjoint_authored_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache");
    let first = BlobCache::new(path.clone());
    let stale = BlobCache::new(path.clone());
    assert!(matches!(first.load(&[1; 32]), BlobLoadOutcome::Missing));
    assert!(!path.exists());
    first.store(&[([1; 32], b"one".to_vec())]).unwrap();
    stale.store(&[([2; 32], b"two".to_vec())]).unwrap();
    let cold = BlobCache::new(path);
    assert!(matches!(cold.load(&[1; 32]), BlobLoadOutcome::Loaded(bytes) if bytes == b"one"));
    assert!(matches!(cold.load(&[2; 32]), BlobLoadOutcome::Loaded(bytes) if bytes == b"two"));
    assert_eq!(
        cold.store(&[([1; 32], b"one".to_vec())]).unwrap().unchanged,
        1
    );
    assert_eq!(
        cold.store(&[([1; 32], b"different".to_vec())])
            .unwrap()
            .conflicts,
        1
    );
    assert!(matches!(cold.load(&[1; 32]), BlobLoadOutcome::Missing));
}

#[test]
fn corrupt_truncated_and_wrong_key_envelopes_never_supply_facts() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf());
    cache.store(&[([1; 32], b"payload".to_vec())]).unwrap();
    let path = dir.path().join(format!("{}.wlc", "01".repeat(32)));
    let original = std::fs::read(&path).unwrap();
    let mut variants = vec![original[..original.len() - 1].to_vec()];
    let mut modified = original.clone();
    *modified.last_mut().unwrap() ^= 1;
    variants.push(modified);
    let mut extended = original.clone();
    extended.push(0);
    variants.push(extended);
    for bytes in variants {
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(cache.load(&[1; 32]), BlobLoadOutcome::Corrupt(_)));
    }
    std::fs::write(
        dir.path().join(format!("{}.wlc", "02".repeat(32))),
        original,
    )
    .unwrap();
    assert!(matches!(cache.load(&[2; 32]), BlobLoadOutcome::Corrupt(_)));
}

#[test]
fn oversized_values_and_unavailable_directories_fail_without_mutating_source() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().join("cache"));
    assert!(
        cache
            .store(&[([1; 32], vec![0; 4 * 1024 * 1024 + 1])])
            .is_err()
    );
    assert!(!dir.path().join("cache").exists());
    let blocked = dir.path().join("blocked");
    std::fs::write(&blocked, "source").unwrap();
    assert!(
        BlobCache::new(blocked.join("cache"))
            .store(&[([1; 32], b"value".to_vec())])
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(blocked).unwrap(), "source");
}

#[test]
fn concurrent_authored_batches_survive_cold_reads() {
    let dir = tempfile::tempdir().unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let threads: Vec<_> = (1..=6u8)
        .map(|id| {
            let cache = BlobCache::new(dir.path().join("cache"));
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                cache.store(&[([id; 32], vec![id; 128])]).unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    let cache = BlobCache::new(dir.path().join("cache"));
    for id in 1..=6u8 {
        assert!(
            matches!(cache.load(&[id; 32]), BlobLoadOutcome::Loaded(bytes) if bytes == vec![id; 128])
        );
    }
}

#[test]
fn held_lock_times_out_and_oversized_headers_are_rejected_before_payload_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_owned());
    cache.store(&[([1; 32], vec![1])]).unwrap();
    let path = dir.path().join(format!("{}.wlc", "01".repeat(32)));
    let original = std::fs::read(&path).unwrap();
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dir.path().join(".write.lock"))
        .unwrap();
    lock.lock().unwrap();
    assert_eq!(
        cache.store(&[([2; 32], vec![2])]).unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
    drop(lock);
    let mut malformed = original.clone();
    malformed[40..48].copy_from_slice(&u64::MAX.to_le_bytes());
    std::fs::write(&path, malformed).unwrap();
    assert!(matches!(
        cache.load(&[1; 32]),
        BlobLoadOutcome::Corrupt(wake_cache::CacheDecodeError::PayloadTooLarge { .. })
    ));
    let mut incompatible = original;
    incompatible[4..8].copy_from_slice(&2u32.to_le_bytes());
    std::fs::write(path, incompatible).unwrap();
    assert!(matches!(
        cache.load(&[1; 32]),
        BlobLoadOutcome::Incompatible { found_schema: 2 }
    ));
}

#[cfg(unix)]
#[test]
fn symlink_directories_and_entries_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let link = dir.path().join("cache");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let cache = BlobCache::new(link);
    assert!(matches!(cache.load(&[1; 32]), BlobLoadOutcome::Io(_)));
    assert!(cache.store(&[([1; 32], vec![1])]).is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}
