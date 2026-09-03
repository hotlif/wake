# ADR 0033: Structured module emit provenance

- Status: accepted
- Date: 2026-09-02

## Context

The compact bundler layout continued to treat emitted JavaScript as a second semantic
representation after typed finalization. It scanned factory bodies to recover dependency IDs for
SCC/topological/concat decisions, guessed export and CommonJS behavior from spellings, rewrote
`module.exports`, `exports`, and `__wake_require__` with whole-string replacement, and injected a
Federation expose identity by finding `runtimeImport(...)` calls in generated text.

Those operations cannot distinguish compiler output from equal bytes in user strings, templates,
comments, or regular expressions. Persistent request metadata also carried traversal-local module
IDs, so a cache hit could pair stable source content with a different generation's numeric graph.
Adding a more complete JavaScript lexer at final layout would create another parser and leave two
semantic owners.

ADR 0024's public-name/star liveness split remains valid, but its eager request deletion and
bounded legacy text-transform plan do not. Correctness takes priority over the small compression
gain from those late textual special cases.

## Decision

1. One optimizer-retained `ModuleEdges` graph is built before liveness filtering. Its ordered
   `ResolvedModuleRequest { specifier, kind, target }` edges are the sole dependency facts for
   liveness, top-level-await propagation and diagnostics, chunk ownership, SCC detection,
   topological order, concat-cycle detection, namespace-identity policy, and CSS cascade order.
   Disabling dead-module elimination retains nodes, not parser-discovered fallback edges.
   `ModuleRec` owns non-edge parser facts such as closed export names, ESM classification, and
   conservative concat safety. Emitted text has no dependency-graph or module-kind semantics.
2. Bundled module planning allocates collision-free runtime symbols for `module`, `exports`, and
   `__wake_require__`, and binds only unresolved runtime references to them. Typed finalization and
   codegen report their final emitted names with each body. Readable, compact, and split-chunk
   wrappers consume those names directly; source bindings (including direct-eval-visible bindings)
   are never renamed by final layout. The sealed typed plan also reports the real runtime
   capability set: `metaUrl`, external require, Promise resolution, Object assign/keys/property
   definition, Federation runtime import, and Federation shared access. Bundled compiler
   intrinsics are exact properties of the typed internal-require binding; the outer wrapper
   installs only the union used by live emitted bodies. Non-bundled preserve finalization instead
   retains its host `require`, `Promise`, and `Object` contract because it has no bundle runtime.
   Default/star interop is structurally inline and has no compact-runtime injection path; only the
   split runtime's private namespace loader retains its own star helper. A non-canonical concat
   candidate conservatively remains an independent factory rather than receiving text aliases.
3. Typed module finalization records every internal request as a target literal `NodeId`, transient
   current-generation module ID, stable source specifier, `ModuleRequestKind`, and role. The same
   typed codegen token walk that writes the body converts the target node to an exact byte range.
   Registered target literals always use canonical base-10 spelling, even when ordinary numeric
   literals are shortened to exponent notation.
4. Final layout may redirect a target only through those typed request facts. It validates the
   complete sorted, non-overlapping range set against the byte-identical body before the first
   splice. One malformed, stale, out-of-bounds, non-canonical, or target-mismatched fact makes the
   entire module a no-op for redirection; partial application is forbidden.
5. Persistent cache schema 12 stores generated request
   `{ range, stable specifier, request kind, role }`, ordered optimizer-retained
   `{ stable specifier, request kind }` identities, the three emitted runtime binding names, and the
   closed runtime capability set paired with the body. It never stores traversal-local target IDs.
   Restore resolves every exact `(specifier, kind)` through the current `EmitLinkerData`, verifies
   the body range is exactly that generation's canonical decimal target, validates sorted and
   non-overlapping ranges plus distinct identifier-safe runtime names, and otherwise misses/rejects
   the complete body-and-metadata contract. No specifier-only fallback is permitted.
6. Federation expose identity is a typed `ModuleLinker`/`EmitLinkerData` fact. It participates in
   body-cache identity, and typed finalization emits it as the optional second `runtimeImport`
   argument even when the import is nested inside another expression or function.
7. The registry/barrel/export/CommonJS scanners and body-name compactor are removed without a
   compatibility path. Structured scope concatenation remains, but an ESM module that directly
   references `module`/`exports` or contains a direct `eval(...)` keeps an owned factory. Empty
   generated text never proves synchronous execution: a typed async fact keeps an empty
   top-level-await factory async. A later size optimization must introduce a typed proof at the
   owning phase.
8. The optimizer identity advances to `wake-closure-minifier-v14`; together with schema 12 this
   prevents reuse of artifacts produced without request-kind provenance, ordered final edges, or
   the complete runtime capability contract.

## Invariants

- User bytes resembling `exports`, `module.exports`, `__wake_require__(7)`, or `_r(42)` cannot
  create graph edges, affect SCC/order/concat, or be rewritten unless their exact numeric literal
  range was emitted from a recorded typed request node.
- Generated request and runtime-name metadata is consumed only with the body from the same codegen
  identity and is validated atomically before mutation or wrapper construction.
- Process-local `NodeId`, `SymbolId`, and numeric module IDs never cross the persistent boundary;
  request condition kind remains part of every stable identity.
- Cache restore maps stable identities into one current generation; unresolved or mismatched facts
  regenerate rather than retaining old IDs.
- Runtime wrapper parameters and body references share final typed-symbol spellings chosen before
  emission, never inferred from emitted JavaScript; user bindings and direct eval retain source
  capture semantics.
- Runtime service installation in readable, compact, and split-entry layouts is the union of typed
  per-body capabilities. Equal bytes in user strings/templates/comments/regular expressions cannot
  request external, Promise, Object, Federation, interop, or `metaUrl` services; no layout
  reconstructs capabilities from text.
- Source-order static requests remain ordered in `ModuleEdges`; set/sorted projections are local to
  algorithms that do not model evaluation or CSS cascade order.
- Federation nested runtime imports are identical across readable/minified and cold/warm paths,
  and changing expose ownership invalidates their body identity.

## Evidence

- `crates/wake_ecma_minify/src/typed_modules.rs` owns request target nodes, request kinds, roles,
  stable specifiers, runtime expose arguments, and compiler-intrinsic capabilities during
  finalization.
- `crates/wake_ecma_codegen/src/typed.rs` produces JavaScript, mappings, exact request ranges,
  final runtime binding names, and the `metaUrl` capability from one finalized typed owner.
- `crates/wake_bundler/src/incremental.rs` constructs and consumes ordered, kind-preserving
  `ModuleEdges` for graph algorithms, validates typed ranges before concat redirection, consumes
  per-body wrapper parameters/capabilities, and carries expose identity through `EmitLinkerData`.
- `crates/wake_cache/src/lib.rs` schema 12 persists stable `(specifier, kind)` identities, runtime
  names, and capabilities and rejects malformed body/metadata contracts atomically.
- Unit and Node end-to-end tests cover request-shaped strings/templates/comments/regex,
  SCC/topology/concat isolation, readable/minified persistent cold/warm execution, malformed cache
  fallback, request-condition divergence across lookup representations, source-order evaluation,
  runtime-name/intrinsic collisions, direct eval/hybrid CommonJS/mixed concat, empty async bodies,
  and nested Federation runtime imports with expose-key invalidation.

## Consequences

Generated JavaScript is again an output artifact rather than a shadow IR. Cache entries survive
generation renumbering only when stable requests resolve to the exact current literals. Removing
late registry/barrel and wrapper-name text compaction increases some minified artifacts and retains
more factory syntax, but removes silent user-data corruption and graph divergence. Request facts
carry a stable specifier string in addition to their small range/role record.

## Validation

- `cargo +1.95.0 test -p wake_cache`
- `cargo +1.95.0 test -p wake_ecma_minify`
- `cargo +1.95.0 test -p wake_ecma_codegen`
- `cargo +1.95.0 test -p wake_bundler`
- `cargo +1.95.0 clippy -p wake_cache -p wake_ecma_minify -p wake_ecma_codegen -p wake_bundler --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `npm run architecture:check`
- `node fixtures/2k-modules/run.mjs`

## Supersedes

[ADR 0024](0024-linker-proven-barrel-compaction.md).

## Removal plan

The emitted-text semantic helpers and their behavior-locking tests are deleted in this change. No
fallback scanner, schema migration, or dual path remains. Future compaction must add typed facts to
the optimizer/codegen contract and advance the relevant cache identity before replacing the
correctness-first factory layout.
