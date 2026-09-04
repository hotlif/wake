# ADR 0041: Cross-process Docs generation transaction

- Status: accepted
- Date: 2026-09-03

## Context

`wake_docs` publishes compiler-generated modules under `.wake/docs/generated` by staging a complete
tree, comparing it with the last accepted tree, moving the old tree to a unique backup, and moving
the candidate into place. A process-local mutex made this serializable for threads in one process,
but independent CLI, Node, development-server, and build processes could inspect the same previous
generation and then interleave their rename and restore sequences. Valid concurrent work could fail,
and an older failed transaction could restore bytes across a newer transaction's commit gap.

The staging and backup directories are siblings of `generated`, so a lock placed inside the generated
tree would be renamed with the tree and would cease to identify one stable transaction domain.

## Decision

`wake_docs` retains its process mutex and additionally opens and exclusively locks the persistent
project-level `.wake/.wake-docs-generation.lock` file before inspecting an accepted generation.
Custom candidate paths containing an earlier `docs/generated` pair are rejected: publishing a
generation inside another generation would invalidate the outer manifest even without concurrency.
The stable project anchor also prevents any accepted generation transaction from moving its lock.
The project lock is held across initial inspection, complete staging, the second comparison, backup,
installation, failure restoration, and post-commit cleanup. It is never removed by publication.

The parent directory is created and its physical namespace is validated before opening the lock;
the namespace is validated again after the lock is acquired. Lock acquisition uses the operating
system file-lock primitive with a bounded 30-second wait. Symbolic links, reparse points, and
non-files are rejected, and the opened handle must still identify the named lock after acquisition.
The operating system releases the lock if a process exits unexpectedly, while the persistent inode
ensures later publishers join the same coordination domain.

Creating the project `.wake` directory is an idempotent first-run operation. If another process wins
the `create_dir` race, the loser reopens and validates the resulting physical directory before it
joins the OS lock; an `AlreadyExists` result is never accepted without that validation.

## Invariants

- At most one process may inspect or mutate any `.wake/**/docs/generated` transaction for a project
  at a time.
- A generated Docs namespace cannot be nested inside another generated Docs namespace.
- The persistent lock is outside every staged, backed-up, and installed generation tree.
- No publisher unlinks or renames the coordination file.
- The coordination path is a physical regular file and must still name the locked inode before the
  generation is inspected.
- Physical ancestor validation runs while the cross-process lock is held and before accepted bytes
  are inspected.
- Rollback and cleanup finish before a waiting publisher can inspect the next accepted generation.
- Lock timeout returns an I/O error without changing the accepted generation.

## Evidence

- `crates/wake_docs/src/lib.rs` owns `acquire_generation_commit_lock` and keeps its guard in
  `publish_generation_with_ops` for the full transaction scope.
- `generated_docs_publication_waits_for_a_separate_process_commit_lock` starts a second test process,
  proves the OS lock is contended, and proves both the standard generation and a legal sibling
  candidate generation cannot complete until that process exits the project transaction.
- `nested_generated_docs_namespace_is_rejected_before_writing` proves overlapping generated-tree
  ownership is rejected before creating either tree.
- `concurrent_first_generation_directory_creation_is_idempotent` covers the missing-`.wake`
  initialization race, while the architecture gate requires the `AlreadyExists` revalidation path.
- `scripts/check-architecture.test.mjs` fixes the lock location and the ordering of inspect, stage,
  replacement, and cleanup as an architecture gate.

## Consequences

Generated Docs candidates for one project serialize across processes, including their staging I/O.
This intentionally favors a correct last-good generation over concurrent materialization into the
same internal namespace. Separate projects use different project lock files and remain independent.
The empty lock file remains under `.wake` as coordination metadata and is not part of the
generated manifest, changed-file inventory, or public build output.

File locks coordinate cooperating processes in the same operating-system filesystem namespace; they
do not claim to defend against another program deliberately replacing Wake's private `.wake`
metadata. A process crash can leave uniquely named staging or backup garbage, but cannot retain the
lock. Crash recovery of an interrupted rename sequence is separate future work.

## Validation

- `cargo test -p wake_docs --lib`
- `cargo clippy -p wake_docs --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `git diff --check`

## Supersedes

None.

## Amends

- [ADR 0038](0038-docs-generation-transaction.md): process-only 并发边界扩展为跨进程事务串行化

## Removal plan

The previous process-only transaction boundary is removed in the same change. No unlocked fallback
or environment-selected lock path is retained.
