# ADR 0028: BuildGeneration owns coherent product publication

- Status: accepted
- Date: 2026-09-02
- Amended by: [ADR 0040](0040-parser-owned-frozen-declaration-graph.md)

## Context

ADR 0027 makes `BuildSession` the sole owner of one typed compilation, but a production application
publication is wider than one compilation. A Federation-enabled candidate contains the application,
container, optional shared provider, declaration bundle, manifest, bootstrap, HTML, assets, and hidden
source maps. If those views create independent filesystems or sessions, they can observe different
moments even when ADR 0026 later publishes their files atomically.

The `FileSystem` contract has no transaction or snapshot primitive. Wake can make repeated observed
facts stable within one product generation, but it cannot truthfully promise a point-in-time snapshot
for paths or query families that have not yet been observed.

## Decision

1. `BuildGeneration` is the sole production owner of every compilation view that contributes to one
   product candidate. It owns one generation-scoped filesystem proxy and creates both retained and
   one-shot `BuildSession` values; product Federation code may not construct its own filesystem or
   session.
2. Ordinary application and Docs production builds compile the application through a generation-owned
   one-shot view. A long-lived application build context stores the `BuildGeneration` beside its
   retained application session. On a watcher batch it advances the generation before invalidating the
   retained application; the Federation container and optional shared provider then compile as
   one-shot children of that same owner.
3. Development Federation does not split the application, exposes, and shared fallback into production
   child builds. One retained dev-server session compiles the synthetic container entry and the
   application roots as a combined graph, and installs a new runtime snapshot only after that complete
   build succeeds.
4. The generation filesystem caches the first completed result, including replayable I/O failures, for
   each exact path spelling in each `FileSystem` method family. `canonicalize`, `read_to_string`,
   `read`, `exists`, `is_file`, `is_dir`, and `read_dir` are deliberately separate families. All
   retained and one-shot views in the generation share those observations; advancing the owner
   replaces the complete cache epoch. Passthrough filesystem decorators preserve the requested
   method family; for example, an ordinary PnP `read_to_string` delegates to the inner
   `read_to_string` rather than silently switching to `read`.
5. Federation configuration, package identities, synthetic entries, and declaration inputs required
   for cross-artifact identity are prepared once for the candidate. Per ADR 0040, declaration
   preparation produces build-independent identity bytes and a frozen-graph binder; after the final
   `buildId` is computed, Wake renders that graph without reading source files again.
6. Wake assembles the full candidate before publication. Application output, container/shared output,
   declarations, manifest, bootstrap, HTML, public assets, hidden source maps, and returned inventory
   either all belong to the new candidate or the previously published generation remains current. The
   candidate crosses the ADR 0026 staging/commit boundary once.
7. The observation cache is explicitly a lazy, query-scoped snapshot, not snapshot isolation. A path
   first queried after the underlying filesystem changes may see the new state, and a first `read` may
   disagree with an earlier `exists` because they are different observation families. Callers that need
   cross-artifact identity must eagerly observe or otherwise freeze those inputs before compilation.
8. A development build failure captures diagnostic source text through the same generation filesystem
   view before emitting `ServerEvent::Diagnostics`. Event consumers derive line and column information
   only from those captured bytes; they do not reopen the host path after the generation boundary.

## Invariants

- One production candidate has exactly one `BuildGeneration` owner.
- The retained application view and transient container/shared views of a build context use the same
  generation epoch.
- Production Federation subbuilds never create `OsFileSystem` or `BuildSession` directly.
- A watcher batch advances the observation epoch before any view observes the next candidate.
- Declaration source is observed once per candidate; final identity rebinding performs no source I/O.
- Diagnostic locations are derived from bytes captured in the generation that produced the
  diagnostic, never from a later host-filesystem read by an event consumer.
- A failed application, Federation, type, materialization, or pre-commit hidden-map step cannot publish
  a partial candidate or replace the last-good generation.
- Generation repeatability applies only to an already observed method/path pair; no global filesystem
  transaction, cross-method consistency, path canonicalization, case folding, or symlink equivalence is
  promised.

## Evidence

- `crates/wake_bundler/src/generation.rs` owns `BuildGeneration`, generation advancement, shared
  retained/one-shot views, per-family observation caches, error replay, and exact `OsString` path keys.
- `crates/wake_resolver/src/pnpfs.rs` preserves the text-query family for ordinary paths while
  retaining archive-byte decoding for zip entries.
- `crates/wake_app/src/lib.rs` creates one generation for application/Docs production candidates and
  stores the generation beside the retained application session in build contexts.
- `crates/wake_app/src/federation.rs` prepares one Federation candidate, uses generation-owned one-shot
  container/provider builds, freezes declarations once, computes one build identity, and assembles all
  Federation artifacts.
- `crates/wake_dev_server/src/lib.rs` compiles the synthetic Federation container, application roots,
  exposes, and shared fallback in one retained mount session and captures build-diagnostic sources
  through that mount's generation filesystem.
- `crates/wake_app/src/lib.rs` converts development diagnostics from captured source bytes without
  reopening their paths.
- `scripts/check-architecture.test.mjs` rejects production Federation filesystem/session bypasses and
  requires the generation owner boundary.

## Consequences

One generation retains cloned successful observations and replayable failures until it advances, adding
bounded memory and synchronization cost. The first observer of a method/path pair defines that fact for
all compilation views in the generation, so generation advancement must remain a product-owned watcher
boundary and must not race an executing build.

The design prevents common cross-build drift without claiming a capability the filesystem abstraction
does not provide. A future true filesystem snapshot can replace the proxy behind `BuildGeneration`
without changing product ownership or publication semantics.

## Validation

- Unit tests exercise same-generation replay across views, all seven query families, failures, exact path
  spelling, concurrent single-flight, generation advancement, and the documented cross-family
  non-guarantee.
- Application tests compare one-shot and retained-context Federation candidates, freeze type identity
  once, inject failures at every materialization stage, and verify the last-good publication remains.
- Dev-server tests prove application/expose/shared-fallback roots share one retained build and that a
  failed combined rebuild does not install a partial runtime snapshot. Generation tests also prove
  runtime/type agreement and diagnostic-source capture from the same cached source bytes.
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `corepack yarn docs:check`
- `git diff --check`

## Supersedes

None.

## Removal plan

Remove direct production Federation construction of `OsFileSystem` and `BuildSession` in the same
migration that introduces the generation owner. Unit-test fixtures may construct isolated sessions and
filesystems after a `#[cfg(test)]` boundary. No production compatibility bridge or second observation
cache remains.
