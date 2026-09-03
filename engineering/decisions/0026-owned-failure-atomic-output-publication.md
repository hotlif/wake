# ADR 0026: Wake-owned failure-atomic output publication

- Status: accepted
- Date: 2026-09-02

## Context

Application and single-site Docs builds previously resolved `outdir` to an absolute path, removed the
entire directory, and then wrote chunks, Federation files, HTML, route shells, and public assets in
separate steps. An `outdir` such as `.`, `src`, or `..` could therefore delete project inputs, while a
late I/O failure left a partially published generation. Aggregated Docs and library builds already had
a reusable staging, backup, and rollback implementation, but ordinary application and Docs leaf builds
did not use it.

## Decision

`wake_app` is the sole output publication owner for application and documentation directory products
and for its exact-file products. It resolves physical output paths, rejects project/input overlap and
link or reparse traversal, and commits through the shared backup-and-rollback engines.

Every exact-file and directory publication uses one process mutex plus one environment-independent,
machine/OS-namespace commit lock named `wake-output-publication-v1`. Windows uses a `Global\\` named
mutex and accepts `WAIT_ABANDONED` as ownership transferred from a terminated writer. Unix uses an
advisory lock on the fixed `/tmp/wake-output-publication-v1.lock` file; it never consults `TMPDIR`,
`TEMP`, or another process-specific environment value. Both primitives release ownership when their
handle/file descriptor or process exits. A bounded 30-second wait fails before target mutation.

Application and Docs candidates materialize in a temporary directory directly below their physical
project root. A valid directory target cannot equal or contain that root, so an ancestor output
publication cannot stale-clean a child candidate before it reaches commit. Library staging already
uses the project root and only publishes its explicitly owned child directories. Exact staging must be
beside each destination for atomic rename, so the global OS lock is acquired before creating any
`.wake-exact-stage-*` file and retained through installation or rollback.

After acquiring the lock, directory publishers repeat physical target, mutation-scope, path-safety,
and ownership validation before deriving the replacement and stale sets. Exact publishers repeat the
complete output/output and output/input identity checks both before staging and immediately before the
first rename. The Unix lock inode is part of those checks: a directory scope containing it or an exact
output aliasing it is rejected. Windows has no filesystem lock inode. The retained
`.wake-output.lock` name is reserved migration metadata: staged directory products and exact products
cannot publish it, while a valid existing regular file with that name is preserved and excluded from
ownership/inventory decisions.

A non-empty output directory is replaceable only when its root contains a valid `.wake-output.json`
whose schema and product match the publisher. New and empty directories are claimed by writing that
marker as part of their first committed generation. Wake does not infer ownership from filenames or a
legacy manifest and provides no force bypass.

## Invariants

1. Project roots, project ancestors, paths containing protected inputs, filesystem roots, symbolic
   links, and Windows reparse points are never directory publication targets.
2. A non-empty directory without a valid matching ownership marker is never cleaned or overwritten.
3. Application chunks, assets, Federation public files, HTML, and manifest are completely materialized
   before the public tree changes. Federation hidden source-map failure also occurs before public commit.
4. Docs bundles, route shells, public assets, workspaces, and aggregate manifest share the same staging
   generation and commit boundary.
5. A failed install or stale-file removal restores the last successfully published file set. This is
   failure atomicity; the current file-by-file installer does not claim lock-free reader snapshot
   atomicity during a successful commit.
6. Generated relative paths cannot be absolute or contain traversal components.
7. Ownership metadata contains no canonical project path and is excluded from public `BuildResult.files`.
8. All `wake_app` exact and directory commits are serialized across threads and participating
   processes in the same OS lock namespace. This includes ancestor/descendant directory targets and
   exact files inside a directory target. A failing writer completes rollback before a later writer
   revalidates or mutates any target.
9. The global commit guard is held across target backup, installation, stale cleanup, and every
   handled-error rollback. It is never dropped between a failed mutation and restoration.
10. Exact publication acquires the global guard before creating same-directory staging files.
    Application and Docs directory staging is physically outside every output tree permitted for that
    project.
11. Publication occurs inside `CancellationToken::commit`. Cancellation observed before that fence
    performs no target mutation. Once a publisher passes the fence, its complete commit or rollback
    linearizes before `cancel()` returns.
12. The lock namespace itself is not publishable: Unix scope/alias checks protect the live global
    inode, and every platform rejects the reserved `.wake-output.lock` migration name.

## Evidence

- `crates/wake_app/src/lib.rs`: physical-path validation, ownership marker, project-root staging,
  cancellation fence, locked revalidation, shared application materializer, and Docs staging
  integration.
- `crates/wake_app/src/federation.rs`: separate staged public files and pre-commit hidden source maps.
- `crates/wake_app/src/lib.rs` tests cover project/source/ancestor rejection, non-empty unowned targets,
  product mismatch, external output, traversal, last-good preservation, stale cleanup, Docs inventory,
  Windows reparse points, injected mid-install rollback, nested parent/child serialization, child
  materialization outside the parent tree, exact staging inside an ancestor directory transaction,
  a separate-process lock holder blocking both directory and exact publishers, and recovery after a
  holder exits without running Rust destructors.
- `crates/wake_app/src/output.rs`: process mutex, Windows named mutex, fixed Unix advisory lock,
  bounded wait, pre-staging exact acquisition, exact-set/lock identity revalidation, and exact
  rollback tests including rejection of lock-path replacement.
- `scripts/check-architecture.test.mjs`: static ordering gate for global lock acquisition,
  pre-staging exact exclusion, project-root directory staging, post-lock revalidation, rollback, and
  cancellation fencing.

## Consequences

Existing non-empty output directories created by earlier Wake versions have no ownership marker. Their
first build after this decision fails with `WAKE_CONFIG`; the user must inspect and remove the old output
or choose a new empty directory. This intentional one-time migration is preferred over guessing that a
directory is safe to delete.

The marker is an internal published file and must not be added to the public output inventory.
Directory products gain an additional staging and backup I/O cost. Exact staging now occupies the
global commit section because its required same-directory placement can otherwise be moved by an
ancestor publication.

Commit concurrency is intentionally conservative: unrelated `wake_app` targets also serialize, while
candidate computation and safe directory materialization remain concurrent. This replaces the former
target-companion design; no lock file is created beside each target or in every writable ancestor.
Existing regular `.wake-output.lock` metadata is left in place and ignored by inventories, but is not
used for coordination.

On Unix the coordination boundary is one OS/mount namespace with a trustworthy shared `/tmp`; a
missing, non-regular, inaccessible, or identity-changing global lock fails closed with `WAKE_IO` or
`WAKE_OUTPUT_COLLISION`. On Windows it is the kernel `Global\\` namespace; an access-control failure
also fails before mutation. Containers with separate mount/kernel namespaces, and non-Wake writers
that ignore advisory locking, are outside this guarantee. Handled I/O failure is rollback-safe, but a
process crash during the file-by-file install is not claimed to be a durable transactional filesystem.

## Validation

- `cargo test -p wake_app --lib`
- `corepack yarn docs:check`
- `corepack yarn architecture:check`
- `git diff --check`

## Supersedes

None.

## Removal plan

The destructive `clean_outdir` path and post-publication Docs leaf writes are removed atomically. No
compatibility bridge or force flag is retained.
