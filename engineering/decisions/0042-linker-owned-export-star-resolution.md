# ADR 0042: Linker-owned `export *` resolution

- Status: accepted
- Date: 2026-09-03

## Context

Typed module lowering previously received only a `preserve_export_star` boolean. It therefore
implemented every retained plain `export *` by enumerating the target namespace at module execution
time and defining non-configurable getters immediately. That representation did not contain enough
information to implement ESM resolution: an explicit export must override every star regardless of
source order, conflicting star bindings are ambiguous, two paths to the same final binding are not
ambiguous, and a cycle cannot discover names from a partially initialized namespace object.
This refines the star-observability portion of [ADR 0024](0024-linker-proven-barrel-compaction.md)
after [ADR 0033](0033-structured-module-emit-provenance.md) moved emission facts to structured typed
boundaries.

## Decision

1. `wake_graph` owns `GetExportedNames`/`ResolveExport`-equivalent analysis over Wake-owned ESM
   modules. It produces one source-ordered plan per plain star declaration.
2. An exact plan contains only names which resolve uniquely through that edge. Explicit names and
   ambiguous names are absent; duplicate paths to the same final binding assign the name to the
   earliest star edge.
3. A module whose transitive star surface reaches CommonJS, an external module, or missing analysis
   is opaque. Its star declarations retain runtime enumeration with all explicit names excluded and
   an own-export collision guard.
4. Typed lowering consumes and validates the plan's ordinal and source specifier. One or two exact
   names become direct live getters; three or more use one callback over a static name array. Both
   forms install getters independently of target initialization state.
5. Static dependency requests remain in source order even when an exact plan forwards no names.
6. Export facts are collected for linked readable builds as well as tree-shaken/minified builds.
   `preserve_export_star` is deleted instead of retained as a compatibility path.
7. Exact names, opaque exclusions, ordinals and specifiers participate in optimizer identity.
   `wake-closure-minifier-v15` invalidates old bodies. Cache schema remains 13 because persisted
   module liveness and body DTO layouts did not change.

## Invariants

- An explicit local or indirect export wins over every plain star independent of statement order.
- Two different final bindings for one star-provided name omit that name from the namespace.
- Multiple star paths to the same final binding emit exactly one public getter.
- Star cycles expose names declared after the cycle edge and do not rely on initialization-time
  `Object.keys` results.
- Removing or statically expanding forwarding never removes or reorders source-module evaluation.
- No `Atom`, `SymbolId`, `NodeId`, AST pointer, or process-local module identity enters persistent
  cache data.
- Cold, retained, and fresh-process warm builds derive the same export-star plan and output bytes.

## Evidence

- `crates/wake_graph/src/lib.rs` owns exact/opaque star planning, final-binding identity,
  ambiguity handling, cycle closure, and unit tests.
- `crates/wake_ecma_minify/src/typed_modules.rs` validates plans and emits direct or shared static
  getters; runtime enumeration exists only for opaque boundaries.
- `crates/wake_bundler/tests/minifier_acceptance.rs` executes readable and minified bundles for both
  source orders, cross-source explicit precedence, ambiguity, same-binding diamonds, cycles, and a
  CommonJS fallback.
- `crates/wake_bundler/src/tests.rs` checks retained replanning and fresh-process persistent-cache
  equality.
- A rebuilt release run of `node fixtures/2k-modules/run.mjs` kept the existing 2k-module baseline:
  1,365,059 raw bytes, 50,922 gzip-9 bytes, 25,076 Brotli-11 bytes, and 1,198 ms average build time.

## Consequences

Wake-owned ESM barrels no longer pay for runtime namespace discovery and cannot redefine an explicit
or already resolved star property. The graph performs bounded recursive resolution over export
surfaces; large exact stars share one generated loop to control raw and compressed size. Opaque
CommonJS/external stars remain necessarily dynamic and conservatively keep first-discovered runtime
bindings when their unknown surfaces collide.

## Validation

- `cargo test -p wake_graph`
- `cargo test -p wake_ecma_minify --lib`
- `cargo test -p wake_bundler --test minifier_acceptance export_star -- --nocapture`
- `cargo test -p wake_bundler persistent_cache_preserves_static_export_star_resolution`
- `cargo test -p wake_bundler retained_rebuild_replans_export_star_names_after_a_source_edit`
- `cargo fmt --all -- --check`
- `cargo clippy -p wake_graph -p wake_ecma_minify -p wake_bundler --all-targets -- -D warnings`

## Supersedes

None.

## Removal plan

No compatibility bridge remains. Runtime enumeration is a permanent explicit fallback only for
opaque CommonJS/external export surfaces; it must be removed from a module automatically when the
graph can prove that module's complete ESM export surface.
