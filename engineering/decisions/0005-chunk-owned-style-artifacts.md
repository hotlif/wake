# ADR 0005: Make extracted styles chunk-owned artifacts

- Status: accepted
- Date: 2026-08-14

## Context

ADR 0004 kept Wake Docs in one production JavaScript chunk because extracted CSS had no owner in
the chunk graph. Loading a lazy route could therefore execute its JavaScript before the route's CSS
was active. That bridge protected correctness but increased the initial JavaScript artifact and made
the product edge responsible for compensating for a missing bundler contract.

## Decision

`wake_bundler` owns extracted styles as chunk artifacts. Every output chunk lists the CSS files that
must be active before its JavaScript executes. The entry HTML loads only entry-owned styles. The
entry runtime serializes the non-entry chunk-to-CSS map, loads dependency chunks first, waits for the
current chunk's stylesheets, and only then loads its JavaScript.

The written build manifest includes the same chunk/style relationship. Wake Docs uses ordinary
production code splitting and no longer configures a single-chunk exception.

## Invariants

- CSS within a chunk follows static dependency evaluation order.
- A dependent chunk's CSS loads after dependency chunk CSS and before dependent JavaScript.
- Entry HTML does not eagerly load styles owned only by lazy chunks.
- Stylesheet and script URLs use the same normalized `publicPath`.
- Server-side and Node chunk loading does not attempt to evaluate CSS.
- Cold, warm-session and persistent-cache builds emit equivalent chunk/style ownership.
- Chunk and style filenames remain content-addressed and deterministic.

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_bundler/src/tests.rs`
- `crates/wake_bundler/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- Browser-shaped VM regression proving CSS load completion precedes lazy JavaScript execution.
- Wake application regression proving production route chunks own their extracted styles.

## Consequences

Wake Docs regains page-level JavaScript splitting and lazy route CSS is not included in the initial
HTML. The runtime adds one deduplicated stylesheet promise per lazy CSS artifact. Build consumers can
inspect chunk/style ownership directly instead of inferring it from the flat asset list.

Independent lazy roots are activated in request order; their global CSS effects therefore follow
activation order. Static dependency order inside each activated chunk graph remains deterministic.

## Validation

- Run the bundler browser-shaped dynamic CSS execution regression.
- Run code-splitting, CSS extraction, public-path and persistent-cache tests.
- Run Wake application and Docs production build checks.
- Compare cold and warm output chunk/style manifests.

## Supersedes

[ADR 0004](0004-style-runtime-and-docs-css-bridge.md)'s production Docs single-chunk bridge. ADR
0004's development style-owner decision remains valid and is not changed by this artifact protocol.

## Removal plan

Completed in the adopting slice: remove `configure_docs_production_bundler`, its
`disable_code_splitting()` call and the single-chunk bridge assertion. No dual CSS loader remains.
