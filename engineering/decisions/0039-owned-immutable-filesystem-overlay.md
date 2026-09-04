# ADR 0039: Owned immutable generated-input overlay

- Status: accepted
- Date: 2026-09-03

## Context

Generated modules must have stable logical identities without exposing staging directories or other
physical files below Wake's reserved namespace. The existing `ProjectedFileSystem` maps logical
paths to a caller-owned physical tree, retains an optional lifetime guard, and uses host
canonicalization when hiding physical paths. Those mechanics are required by current callers, but
they cannot prove that a build sees exactly one sealed generation or that an undeclared physical
file below the logical root is invisible.

The primitive must also support an application-wide migration without changing stable logical
module identities or the legacy Docs publication API. Application, Docs, and Federation producers
must contribute bytes before a bundler generation or retained session can observe the tree.

## Decision

`wake_common` provides a typed, immutable, single-root overlay:

1. `ProjectedRelativePath` accepts only a non-empty sequence of Normal relative components and
   rejects absolute, root, prefix, `.` and `..` inputs.
2. A non-cloneable `OwnedFileTreeBuilder` accepts bytes through `&mut self`, rejects duplicate,
   case-equivalent and file/directory-conflicting identities, and is consumed by `seal`.
3. `OwnedFileTree` owns every payload as `Arc<[u8]>`, is cloneable after sealing, and exposes a
   stable sorted inventory and zero-copy shared iterator.
4. `OwnedOverlayFileSystem` owns exactly one normalized logical root. Every `FileSystem` operation
   inside that root is answered exclusively by the sealed tree. Directories and listings are
   derived from declared files; undeclared physical entries never fall through to the base.
5. Ancestor listings remove a base entry matching the owned root and synthesize the next logical
   child when the sealed tree is non-empty. Ancestor synthesis never crosses the absolute/relative
   path-domain boundary.
6. `FileSystem::canonicalize` resolves identity in the active filesystem's logical namespace.
   Owned paths return their normalized logical identity without touching the base; projected and
   PnP paths validate their backing object without leaking physical staging or archive paths.
   `BuildGeneration` caches this seventh query family beside content and metadata observations.

The overlay performs lexical platform-aware comparisons for its owned namespace. Its canonicalize
operation does not inspect shadowed host paths; paths outside the owned root retain the base
filesystem's identity semantics. The overlay does not borrow a physical generation tree or carry a
separable lifetime guard.

Application orchestration follows a render/compose/seal/bind protocol:

1. A non-cloneable `GenerationDraft` collects virtual entry, component scan, Docs, and Federation
   inputs. `GenerationView` wraps one `Arc` containing the sealed tree and its filesystem; clones
   therefore retain one inseparable byte/capability identity and expose no append operation.
2. `wake_docs::render_with_mode` is a pure renderer returning `RenderedProject` and an
   `OwnedFileTree`. Existing `generate*` APIs adapt this result to the ADR 0038 physical publication
   transaction for compatibility; application build/dev paths consume the owned tree directly.
3. Federation production first renders config-only sources, the app merges and seals them with
   core inputs, and only then creates `BuildGeneration`. A read-only bind phase resolves packages,
   prepares types, and captures the lock through that generation filesystem. Artifact construction
   performs no later lock read. Development captures one lock object for type sync and runtime
   bootstrap and merges its generated wrappers before publishing the mount plan.
4. Watch coverage may materialize a draft to discover refined interests, but final activation
   re-renders after the wider revision is covered. Candidate sessions are created only after that
   coverage fence. Failed candidates retain their immutable view/session for retry; accepted views
   are replaced atomically.
5. Exact bundle publication excludes declared owned inputs from host canonicalization because they
   deliberately have no host file. Bundle destinations are independently rejected beneath the
   reserved `.wake` namespace, preserving input/output disjointness.
6. Each development mount retains one `BuildGeneration` beside its `BuildSession`. An accepted
   watcher invalidation advances both epochs together, and the runtime build returns the current
   generation filesystem view as part of the same owner operation. Federation declaration
   preparation receives that exact view; it cannot reopen the mount's mutable source filesystem.

## Invariants

- A sealed tree's bytes and inventory cannot change when the builder, renderer, or host filesystem
  changes.
- All seven `FileSystem` methods, including logical canonicalization, avoid the base filesystem for
  the owned root and every descendant.
- The owned root is fail-closed: a missing declaration is `NotFound`, even when the base contains a
  file at the same logical path.
- Owned directories are the strict ancestor closure of declared files; file/directory collisions
  cannot be sealed.
- Stable inventory order is independent of insertion order, and shared payloads can be composed by
  cloning `Arc` without copying bytes.
- Platform-equivalent identities use one spelling. On Windows, case-equivalent file or directory
  aliases are rejected before sealing.
- Relative queries cannot discover an absolute owned root, and absolute queries cannot discover a
  relative owned root.
- Every product session observes one complete generated-input tree; `BuildGeneration` is never
  created before all product inputs are composed.
- A development Federation runtime and its frozen declaration graph observe the same mount/query
  snapshot. The next watcher batch replaces both the generation query cache and retained bundler
  generation before either product is rebuilt.
- Generated files below `.wake` are absent from the host filesystem in build, watch, dev, lazy
  workspace, and Docs application paths when optional persistent cache/output artifacts are off.
- A Federation generation reads its remote lock at most once and retains that parsed snapshot for
  bootstrap construction.

## Evidence

- `crates/wake_common/src/fs.rs`: `ProjectedRelativePath`, `OwnedFileTreeBuilder`,
  `OwnedFileTree`, `OwnedOverlayFileSystem`, and logical canonicalization implementations.
- `crates/wake_bundler/src/generation.rs`: generation-scoped canonical path observation and replay.
- `crates/wake_app/src/lib.rs`: `GenerationDraft`, `GenerationView`, product input composition,
  watch coverage fencing, logical generation diffs, and virtual-input-aware exact publication.
- `crates/wake_app/src/federation.rs`: `ProductionFederationInputs`,
  `render_production_inputs`, `bind_production_generation`, captured lock ownership, and the
  generation-filesystem declaration callback used by development.
- `crates/wake_dev_server/src/lib.rs`: `MountBuildSession`, paired generation/session invalidation,
  and runtime-output/filesystem-view handoff to Federation assembly.
- `crates/wake_dev_server/src/federation.rs`: declaration preparation requires the filesystem view
  supplied by the mount generation owner.
- `crates/wake_docs/src/lib.rs`: `RenderedProject`, `render_with_mode`, and compatibility
  render-then-publish adapters.
- Unit tests use a base filesystem that panics on owned-root access and cover all seven methods,
  physical rogue files, typed path rejection, collisions, Windows case aliases, ancestor listings,
  host mutation after sealing, stable clones, zero-copy shared payloads, and canonical identity
  replay until generation advance.
- Application tests cover stale watch revisions, refined-coverage re-rendering, failed-candidate
  identity reuse, logical added/modified/removed diffs, lazy workspace materialization, no physical
  candidate tree, exact-output rollback, overlay-only Federation declarations, pure Federation
  rendering, and single-read lock snapshots.
- Development tests make the underlying source return different bytes on consecutive reads and
  prove runtime/declaration agreement within one epoch plus joint movement after invalidation. The
  architecture gate pins the paired mount owner and the exact view handoff into Federation.

## Consequences

Tree construction currently performs linear conflict checks per insertion. Generated inventories
are expected to be modest, and fail-closed identity validation is preferred over a second mutable
index in this slice. If measurements show this is material, a private platform-key index may be
added without changing the public ownership contract.

The overlay intentionally does not resolve symlinks or Windows reparse points. Consumers construct
logical roots from an already-validated canonical project identity; treating a physical alias as a
second logical identity is outside this primitive.

Persistent cache files remain optional derived host state below `.wake` and are deliberately hidden
from the generation overlay. Thus “no physical `.wake`” applies when cache and separately published
Federation output artifacts are disabled. Federation expose canonicalization and declaration
preparation still occur during the read-only bind phase; parser-owned declaration facts are a
separate follow-up boundary and this decision does not claim they are config-only.

## Validation

- `cargo fmt --check`
- `cargo check -p wake_common`
- `cargo test -p wake_common --lib`
- `cargo clippy -p wake_common --all-targets -- -D warnings`
- `cargo test -p wake_bundler generation::tests`
- `cargo test -p wake_resolver pnpfs::tests`
- `cargo test -p wake_dev_server federation_runtime_and_types_share_one_mount_generation_snapshot`
- `cargo check -p wake_app --all-targets`
- `cargo test -p wake_app --lib`
- `cargo test -p wake_docs`
- `cargo clippy -p wake_app --all-targets -- -D warnings`
- `git diff --check`
- `corepack yarn architecture:check`
- `corepack yarn architecture:test`

## Supersedes

None.

## Amends

- [ADR 0037](0037-typed-development-watch-and-candidate-generations.md): decision 8 的物理 candidate generation 改为 owner 持有的不可变生成输入 overlay

## Removal plan

No product path uses `ProjectedFileSystem` or a physical candidate generation after this migration.
The older projection type remains exported only as an internal Rust compatibility surface while
the repository does not publish a Rust stability contract for `wake_common`; it may be removed in a
dedicated cleanup once downstream workspace history no longer needs it. The legacy public Docs
`generate*` functions remain intentionally and publish through the ADR 0038 transaction; they are
not used to feed application bundler sessions.
