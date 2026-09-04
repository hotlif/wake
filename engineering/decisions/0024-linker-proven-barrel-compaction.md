# ADR 0024: Linker-proven barrel and trivial-module compaction

- Status: accepted
- Date: 2026-08-31
- Amended by: [ADR 0033](0033-structured-module-emit-provenance.md)
- Amended by: [ADR 0042](0042-linker-owned-export-star-resolution.md)

## Context

The minified `fixtures/2k-modules` bundle was correct and faster to build than Vite, but measured
434,303 raw bytes versus Vite's 164,605. Artifact attribution found two independent sources:

- side-effect-only barrel modules emitted hundreds of runtime `export *` namespace enumeration and
  getter loops even when the linker proved that no public name was consumed;
- the binding-free optimizer exits skipped the late syntax normalization which turns safe computed
  identifier properties into dot properties, preventing the registry compactor from coalescing
  repeated bootstrap expressions.

Module-local `SymbolId` roots alone cannot prove a star export dead. A named request can pass through
`export *` without creating a symbol in the barrel, and a static request must still execute its target
for top-level effects after forwarding is removed.

## Decision

1. `compute_live_keep` reports three independent facts for exact modules: export names whose local
   declaration bindings must remain live, public export keys actually observed by another module,
   and whether a consumed name resolves through a plain `export *` path. Missing analysis, unknown
   exports, namespace use, dynamic import, `require`, and entry observation continue to produce the
   conservative `All` result.
2. `LinkerExportLiveness` carries stable retained and observed name sets plus star observability.
   Only retained names resolve to parser-generation `SymbolId` roots; observed names remain strings
   through `TypedModuleOptions` and independently control public getter emission.
3. Scope concatenation removes a standalone internal request only when its target is also a member of
   that concat. Requests to standalone factories remain executable.
4. The two proven binding-free minifier exits run exactly one `LatePeephole` pass before plan sealing.
   They still skip semantic rebuild, the full fixed point, inlining, DCE, and mangling.
5. Typed module finalization records a static request only when it is compiler-generated, internal,
   synchronous, Program-top-level, and its value is structurally discarded (direct expression
   statement or root sequence element). `require`, dynamic/external/async and value-producing sites
   are excluded.
6. Typed codegen converts those process-local `NodeId` proofs into sorted exact body-byte ranges with
   stable real target module IDs. Final minified layout replaces a range with `0` only when that
   target is in the explicit eager registry candidate set. The replacement happens before every
   text compaction/redirect pass and retains surrounding sequence grammar.
7. Generated request ranges participate in `EmittedBody` hashing and persist beside mapping facts;
   cache schema advances to 10. Malformed cached ranges make the optimization a no-op.
8. Synthetic concat factory IDs are allocated above the maximum real module ID. They never share
   the real module-ID domain by an arithmetic estimate.
9. The optimizer identity advances to `wake-closure-minifier-v12`.
10. Explicit named/default/namespace re-exports are filtered by their exported public key before a
    namespace binding is allocated. If no key survives, typed planning emits only the original
    source-ordered static request. Plain `export *` forwarding uses the independent star fact.

## Invariants

- Unknown or namespace-shaped export observation never takes the empty-star path.
- Removing forwarding never removes dependency evaluation, changes source order, or drops external
  `require` effects.
- A named import through one or more star barrels remains observable at every barrel boundary.
- `export * as namespace` retains namespace construction when that public key is observed; the dead
  form still evaluates its source module.
- Fast binding-free compaction remains linear in the residual owned IR and does not build semantics.
- Semantic choices remain graph/typed-IR facts; final emitted text is not used to infer export
  liveness.
- Text cannot manufacture request provenance. Final layout may only filter proof-carrying generated
  ranges by its explicit eager-target set.
- Async requests, used request values and non-candidate targets retain their runtime calls.
- Generated ranges are consumed against the byte-identical body which produced them and before any
  byte-changing linker rewrite.
- Synthetic runtime IDs are outside the complete real module ID range.
- Retained names, observed names, the star fact and the pipeline version participate in cache
  identity.

## Evidence

- `crates/wake_graph/src/lib.rs`: `LiveResult::Names` keeps declaration retention, exact public
  observation and star forwarding as separate fields.
- `crates/wake_ecma_minify/src/typed_modules.rs`: exact-empty star and explicit re-export lowering
  preserve static requests while omitting dead namespace bindings and getters.
- `crates/wake_ecma_minify/src/typed_pipeline.rs`: both trivial exits share one late peephole pass.
- `crates/wake_ecma_minify/src/typed_modules.rs`: static/discard/sync/top-level eligibility is proved
  against typed parents during finalization.
- `crates/wake_ecma_codegen/src/typed.rs`: finalized request nodes become exact generated ranges on
  the same token walk as mappings.
- `crates/wake_cache/src/lib.rs`: schema 10 round-trips request ranges with body metadata.
- `crates/wake_bundler/src/incremental.rs`: final eager candidate membership owns replacement;
  concat request removal remains target-membership checked and synthetic IDs are collision-free.
- Graph, typed-module, minifier, and Node end-to-end tests cover retained-vs-observed aliases, named
  star consumption, exact-empty forwarding removal, retained target effects, and both trivial exits.
- Before structured request compaction, the Wake artifact was 99,257 raw / 24,107 gzip-9 / 10,718
  Brotli-11 bytes. After it, the no-cache five-run fixture averaged Wake 255 ms, Vite 483 ms and
  webpack 5,155 ms; peak memory averaged 110 / 195 / 550 MB. The Wake artifact became 81,258 /
  16,682 / 9,350 bytes and retained checksum `modules=2013 hash=2876300985`; Vite was 164,605 /
  12,952 / 7,085 bytes and webpack was 170,930 / 15,320 / 8,936 bytes.
- After public-name getter filtering, the same no-cache five-run fixture averaged Wake 255 ms, Vite
  484 ms and webpack 5,079 ms; peak memory averaged 106 / 195 / 550 MB. Wake became 76,992 raw /
  16,133 gzip-9 / 8,919 Brotli-11 bytes, its `Object.defineProperty` sites fell from 81 to 39, and
  the Node checksum remained unchanged.

## Consequences

Wake keeps its cold-build lead. Repeated high-entropy runtime target IDs no longer survive at
discarded imports of eagerly executed registry-only modules; a literal `0` keeps grammar and gives
gzip/Brotli a uniform token. Used namespace requests and all non-eager factories keep their runtime
semantics. Each binding-free module pays one small syntax-only traversal, and every non-trivial body
carries a small range vector until final layout. The graph result carries star-consumed public names
that were previously omitted. Structured request compaction removed 17,999 raw / 7,425 gzip /
1,368 Brotli bytes; the independent public-name slice then removed another 4,266 / 549 / 431 bytes
and 42 getter sites while keeping Wake 1.9× faster than Vite. Wake is now 87,613 raw bytes smaller
than Vite, while Vite remains 3,181 gzip and 1,834 Brotli bytes smaller. Attribution shows the
remaining gap is dominated by whole dead factory bodies, not registry representation or request-ID
micro-syntax; deleting those bodies requires a separate graph-proof slice.

## Validation

- `cargo +1.95.0 test -p wake_graph`
- `cargo +1.95.0 test -p wake_ecma_minify`
- `cargo +1.95.0 test -p wake_bundler --test minifier_acceptance`
- `cargo +1.95.0 clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `node fixtures/2k-modules/run.mjs`

## Supersedes

None.

## Removal plan

No compatibility bridge is introduced. Standalone eager-candidate request deletion no longer uses a
text scanner. Generated barrel compaction and concat-member request compaction remain bounded legacy
text transforms and should receive their own typed/codegen metadata slices; they must not infer the
new request provenance from emitted strings.
