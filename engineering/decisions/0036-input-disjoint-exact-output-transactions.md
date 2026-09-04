# ADR 0036: Input-disjoint exact-output transactions

- Status: accepted
- Date: 2026-09-02

## Context

ADR 0026 established one publication owner and shared commit lock for directory and exact-file
products, but exact-file callers still bypassed its complete-set transaction and authoritative
input-provenance boundary.
`bundle({ outfile, sourceMap: true })` wrote the map and JavaScript with two independent
`atomic_write` calls. A failure after the first replacement produced a cross-generation pair. The
outfile was also only joined to the project root, so it could be the entry, a transitive module, a
resolver/configuration input, a hard link, or a symbolic alias of one.

`generateCssToken` had the same collision: `[build].output` could replace the root `token.toml`, a
recursively imported token configuration, or PnP metadata/archive bytes used to resolve it. The
fixed `generateDocgen` output could likewise replace its entry or a recursively read local type
source. Per-file atomic replacement prevented torn bytes, but did not provide set atomicity or
input/output separation.

## Decision

1. `wake_app::output` is the sole exact-file publication owner. Callers submit the complete write
   candidate set and the authoritative successful-content-read set to `publish_exact_outputs`.
   No exact product publishes one member before constructing the others.
2. Before creating a directory or temporary file, every destination is compared with every input
   and every other destination by normalized lexical path, canonical physical path, and open-file
   identity (`same-file`, including Windows volume/file identity and hard links). Missing
   destinations are projected through their deepest canonical existing ancestor. A previously read
   ordinary input that disappears before publication fails closed; PnP zip virtual paths retain a
   lexical identity while the physical archive read is independently recorded.
3. Validation first runs before any parent creation. Publication then enters ADR 0026's shared
   cross-process output lock, validates again, creates missing parents, and repeats complete-set and
   lock-identity validation before staging. Every payload is written to a uniquely named temporary
   file in its destination directory, then flushed and synchronized; validation repeats immediately
   before mutation. Existing destinations are moved to unique same-directory backups before the
   first new file is installed. Any backup or install failure removes installed candidates and
   restores all backups in reverse order while the same lock remains held.
4. The commit point is successful installation of the entire candidate set. Backup removal is
   post-commit garbage collection: it is retried and a uniquely named backup may be retained if the
   OS refuses cleanup. Such cleanup cannot honestly be reported as a failed publication because an
   earlier deleted backup can no longer support rollback.
5. Exact bundle publication submits JavaScript and, when requested, its rewritten source map in one
   transaction. Disabling source maps does **not** delete an adjacent stale `.map`, because an exact
   outfile has no ownership marker proving that companion belongs to Wake. The returned inventory
   contains only files installed by the successful operation.
6. Bundle compilation runs over `RecordingFileSystem`, so entry, transitive modules, package
   manifests and PnP/archive content reads become protected inputs. Application configuration,
   browserslist inputs and the entry are added at the application boundary. Token resolution builds
   its `ResolutionEnvironment` over the same recorder, covering root/imported token files and
   resolver/PnP reads. `wake_tsdoc::extract_component_api_with_provenance` returns every recursive
   source read, and Docgen combines it with entry-resolution reads before publication.
7. Input/output collision is a stable `WAKE_OUTPUT_COLLISION` error. It is checked only after a
   candidate has been generated successfully but always before a destination changes; every
   protected input and unrelated sentinel therefore retains its bytes.
8. Every exact caller invokes publication inside `CancellationToken::commit`. Cancellation before
   the fence performs no destination or staging mutation; after the fence, publication or rollback
   linearizes before `cancel()` returns.

## Invariants

- No exact output can be the same file as a successful content input through spelling, case,
  canonicalization, symbolic/reparse traversal, or hard-link identity.
- JavaScript and its emitted source map are either both from the new build or both from the previous
  build after any reported publication failure.
- Every staged payload is same-directory, complete, flushed and synchronized before a destination
  backup or install begins.
- The shared OS commit lock is acquired before any `.wake-exact-stage-*` file exists and remains
  held through handled-error rollback. An ancestor directory publisher therefore cannot move exact
  staging, and an exact output cannot replace the Unix lock inode or reserved migration lock name.
- A failed backup/install restores every old destination byte and removes newly created
  destinations; unrelated files are never touched.
- Token and Docgen input provenance is produced by the reader/resolver, not reconstructed from a
  guessed list after generation.
- A successful inventory names exactly the candidate payloads installed by that call. Unowned stale
  companions are preserved and excluded.

## Evidence

- `crates/wake_app/src/output.rs`: recording filesystem, locked repeated identity validation,
  pre-staging global lock acquisition, same-directory staging, backup/install/rollback, reserved
  lock rejection, and cross-platform alias tests.
- `crates/wake_app/src/lib.rs`: bundle read provenance, full code/map candidate construction and
  exact publication tests for entry, transitive input, map collision, locked destinations and
  unowned stale maps.
- `crates/wake_app/src/library.rs`: token resolver provenance, Docgen entry-resolution provenance,
  and collision tests for recursive token/type inputs, PnP metadata and symlink aliases.
- `crates/wake_tsdoc/src/lib.rs`: authoritative recursive component-extraction read provenance.
- `scripts/check-architecture.test.mjs`: prevents exact products from returning to independent
  `atomic_write` calls or guessed token/Docgen input lists, and locks the order of cancellation
  fence, OS lock, staging, final revalidation, mutation, and rollback.

## Consequences

Exact file publication performs temporary writes plus backup renames and holds ADR 0026's global
process-and-OS Wake commit lock, including while staging. This favors correctness over maximum
concurrency for a very small candidate set and serializes exact publication with directory
publication in participating processes. The OS/mount namespace and advisory-participant limits in
ADR 0026 still apply; repeated identity validation does not claim to eliminate malicious external
filesystem races or provide lock-free reader snapshot atomicity.

`same-file` is an application-edge dependency, not a compiler-core dependency. Source-map-off
builds may leave an old adjacent map; removing it requires a future explicit ownership record, not
filename inference. A refused post-commit backup cleanup may leave a uniquely prefixed recovery
file, while the requested destination set is already fully committed.

## Validation

- `cargo +1.95.0 test -p wake_tsdoc`
- `cargo +1.95.0 test -p wake_app`
- `cargo +1.95.0 clippy -p wake_tsdoc -p wake_app --all-targets -- -D warnings`
- `cargo +1.95.0 fmt --all -- --check`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `corepack yarn docs:check`
- `git diff --check`

## Supersedes

None.

## Amends

- [ADR 0026](0026-owned-failure-atomic-output-publication.md): 补齐 exact-file 的完整候选集、输入 provenance 与成组回滚事务

## Removal plan

The independent bundle map/JavaScript writes and direct token/Docgen writes are removed in this
change. No compatibility writer, force-overwrite flag, guessed provenance fallback, or unowned stale
map deletion remains.
