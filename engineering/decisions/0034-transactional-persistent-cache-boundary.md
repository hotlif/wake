# ADR 0034: Transactional persistent-cache boundary

- Status: accepted
- Date: 2026-09-02

## Context

The persistent build cache was treated as an untyped best-effort file. Load collapsed missing,
incompatible, corrupt, and I/O failures into the same empty value. Store used one fixed `.tmp`
name and, when rename failed, truncated the live cache in place. Concurrent Wake processes could
lose one another's entries. The decoder accepted trailing bytes and loose boolean/tag values,
trusted unbounded collection counts, silently overwrote duplicate keys, and had no checksum.

The cache also persisted `path -> (mtime, size, source)` snapshots. A fresh process could therefore
reuse stale source when another tool replaced a file while preserving its metadata. This made an
optional performance layer a source-of-truth authority. `wake_app` also created `.wake` before the
cache transaction, turning an unavailable optional cache directory into a fatal build error, while
successful watch rebuilds discarded non-error diagnostics.

## Decision

1. Persistent schema 13 contains derived summaries, optimizer-retained request identities, and
   atomic body/mapping emission groups. It contains no paths, file metadata, source text, AST,
   interner identity, or generation-local module IDs. Every fresh process reads loader output and
   hashes the real source before consulting derived entries. In-process load-task reuse is
   unaffected.
2. A cache file has a fixed 32-byte little-endian envelope: four-byte magic, `u32` schema, `u64`
   payload length, and an XXH3-128 checksum. The checksum covers magic, schema, length, and payload,
   but not its own field. The decoder checks the declared length before allocation, enforces
   aggregate entry/item/owned-byte budgets with checked arithmetic and fallible reservation,
   requires exact boolean and enum tags, rejects duplicate keys and invalid semantic combinations,
   and requires exact end-of-file.
3. Load returns one typed outcome: loaded, missing, incompatible, corrupt, or I/O. Missing and
   incompatible are normal silent misses. Corruption and I/O remain correctness-neutral misses but
   are observable as a `WAKE_CACHE` warning.
4. Store owns creation of its parent directory. It takes a bounded exclusive lock on a companion
   `<cache>.lock` file, reloads the latest durable schema under that lock, and overlays only keys
   authored by the current process. Equal immutable facts coalesce. Conflicting summary or
   retained-request keys are removed. A body and its mappings are one provenance group: a conflict
   or complementary half from different writers removes the complete group rather than inventing
   a pairing.
5. After deterministic key-sorted compaction and encoding, store writes a uniquely named temporary
   file in the destination directory, flushes and synchronizes it, then atomically replaces the
   destination. There is no direct-write fallback. Any create, lock, reload, encode, write, sync, or
   replace failure leaves the previous file intact and the in-memory cache dirty for a later retry.
   Dirty state clears only after replacement succeeds.
6. Cache failures never fail a build. One build emits at most one `WAKE_CACHE` warning, combining
   load/store/repair/conflict detail when necessary. Successful one-shot, plain-watch, TUI-watch,
   Rust, and Node-facing build results retain that diagnostic; only error-severity diagnostics
   affect build success.
7. `wake_app` computes the optional cache path but performs no eager cache-directory mutation. The
   cache transaction is the only owner of its directory and file lifecycle.

## Invariants

- Source bytes are always newer authority than persistent data; preserved mtime and size cannot
  cause stale output.
- A malformed, oversized, truncated, extended, duplicate-key, checksum-mismatched, or semantically
  invalid schema-13 file contributes no cached facts.
- No store failure truncates or partially replaces the last durable cache.
- A stale writer cannot resurrect entries it merely loaded, and concurrent writers cannot create a
  body/mapping pair that neither writer produced.
- Deterministic logical cache contents have deterministic bytes independent of `HashMap` insertion
  order.
- Cache availability changes performance and warnings only, never emitted application semantics or
  process success.

## Evidence

- `crates/wake_cache/src/lib.rs` owns the envelope, bounded codec, typed outcomes, authored overlay,
  lock, deterministic merge, atomic emission group, and store transaction.
- `crates/wake_bundler/src/incremental.rs` reads source before content-key lookup, maps cache
  outcomes into bounded warnings, and commits body/mappings through one cache operation.
- `crates/wake_bundler/src/loader.rs` has no persistent source-type shortcut.
- `crates/wake_app/src/lib.rs` only computes cache paths and preserves successful diagnostics.
- `crates/wake_cli/src/main.rs` records successful watch diagnostics in plain and TUI modes.

## Consequences

Fresh processes perform one real source read and content hash per module, so the cache no longer
claims to eliminate source I/O. They can still skip parse, optimizer, and body codegen work. A
companion lock file remains on disk and lock acquisition waits for at most 500 ms before falling
back with a warning. The current encoder builds a payload and envelope buffer concurrently, so its
peak memory can approach two encoded payloads; schema budgets cap each payload at 512 MiB, and a
future streaming/in-place encoder may reduce that peak without changing the format.

Schema migration is intentionally absent: older and newer schemas are silent misses because all
entries are derivable. Corrupt-current repair and merge conflict removal are reported, then normal
build execution supplies authoritative replacements.

## Validation

- `cargo +1.95.0 test -p wake_cache`
- `cargo +1.95.0 test -p wake_bundler`
- `cargo +1.95.0 test -p wake_app`
- `cargo +1.95.0 test -p wake_cli`
- `cargo +1.95.0 clippy -p wake_cache -p wake_bundler -p wake_app -p wake_cli --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `yarn architecture:test`

## Supersedes

None.

## Removal plan

The schema-12 codec, fixed temporary filename, direct-write fallback, path/source snapshot DTOs,
and split body/mapping store calls are removed in this change. There is no compatibility decoder or
dual source-authority path.
