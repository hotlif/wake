# ADR 0027: BuildSession owns product compilation and engine lifetime

- Status: accepted
- Date: 2026-09-02

## Context

`wake_bundler` exposes both `BuildSession` and the mutable `IncrementalBundler`. Product callers in
`wake_app` and `wake_dev_server` configure the latter through setter sequences and sometimes call it
directly, while other paths wrap the configured engine with `BuildSession::from_incremental`. This
creates multiple public state-transition paths. It also lets a one-shot engine be wrapped by a
constructor that unconditionally enables the retained load cache, disabling one-shot terminal fast
paths and retaining/cloning output that a cold build could return by ownership.

The mutable setter surface additionally makes it possible to omit a semantic option from one product
path or change it after task nodes have been created. The persistent source cache demonstrated this
risk when `css_in_js` was absent from its outer variant identity.

## Decision

1. `BuildSession` is the only public product compilation owner in `wake_bundler`. The underlying
   `IncrementalBundler` remains an implementation detail and may be tested directly only inside the
   crate.
2. A session is created from one immutable, owned `BuildOptions` value. JSX mode/import source,
   entry naming, single-chunk hashing, persistent-cache location and the complete bundler-facing
   Federation plan are typed options rather than post-construction product setters.
3. Retained and one-shot lifetimes are explicit constructors. A retained session enables the load
   cache and owns committed generation state. A one-shot session keeps the transient engine fast
   path and exposes a consuming `build_once`, so it neither permits a second build nor clones the
   completed output into committed state.
4. Forced retained builds and current-generation builds share one commit transition. A successful
   forced build advances the generation and replaces the committed entry/output; a later
   `build_current` observes that exact result.
5. Every semantic loader/parse/optimize/emit input participates in the nearest reusable identity.
   In particular, the persistent source variant includes the owned JSX identity, target identity and
   CSS-in-JS mode before it may restore a cached content key.
6. `Bundler` may remain as a compatibility facade only when it delegates to typed `BuildSession`
   construction. Product crates may not import `IncrementalBundler` or use a configurator closure to
   recreate the setter escape hatch.

## Invariants

- A product build enters `wake_bundler` through one typed session API.
- Session configuration cannot change after construction.
- One-shot construction does not enable retained load caching and one-shot output is moved once.
- Retained generation, committed entry and committed output change in one state transition.
- A fresh-process persistent-cache hit is byte-for-byte and graph-for-graph equivalent to a cold
  build under the same complete semantic options.
- No configured string is leaked to manufacture a `'static` JSX lifetime.

## Evidence

- `crates/wake_bundler/src/session.rs` owns typed options, lifetime-specific construction and the
  shared retained commit transition.
- `crates/wake_bundler/src/incremental.rs` includes CSS-in-JS in the persistent source variant and
  keeps the transient one-shot engine behavior private.
- `crates/wake_bundler/src/tests.rs` compares cross-process CSS-mode cache reuse with a cache-free
  cold build.
- `crates/wake_bundler/tests/one_shot.rs` compares retained and one-shot output fields while using
  only the public session surface.
- `scripts/check-architecture.test.mjs` rejects product imports of `IncrementalBundler` and
  `BuildSession::from_incremental`.

## Consequences

Callers must construct a larger options value, but compilation lifetime and semantic identity are
reviewable at one boundary. One-shot builds retain their lower allocation/retention behavior, while
watch and development builds retain explicit generation state. Adding a new semantic setter now
requires adding it to typed options and its cache identity rather than silently creating another
product path.

## Validation

- Run `cargo +1.95.0 test -p wake_bundler --lib` and the one-shot, minifier and performance
  integration suites.
- Run `cargo +1.95.0 test -p wake_app --lib` and `cargo +1.95.0 test -p wake_dev_server`.
- Run all-target Clippy for the affected Rust crates with warnings denied and compile the bundle
  benchmark without running it.
- Run `corepack yarn architecture:test`, `corepack yarn architecture:check` and
  `git diff --check`.

## Supersedes

None.

## Removal plan

Remove `BuildSession::from_incremental`, the public `IncrementalBundler` re-export and product-side
`BundlerLifetime`/setter factories after all production callers and external integration tests use
typed session construction. No deprecated product path remains after that atomic migration.
