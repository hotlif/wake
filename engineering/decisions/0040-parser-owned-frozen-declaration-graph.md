# ADR 0040: Parser-owned frozen declaration graph

- Status: accepted
- Date: 2026-09-03

## Context

Wake emits TypeScript declarations for libraries and Federation remotes. The original path treated
those declarations as formatted text: `wake_tsdoc` discovered exports with line-oriented regular
expressions, while `wake_app` separately scanned strings to find module requests, remove ambient
keywords, and reject public `any`. Federation identity rebinding then replaced a reserved namespace
throughout complete declaration bodies and JSON bytes.

Those operations cannot distinguish syntax from comments, string literal types, property names, or
template interpolation. They also make same-line exports, overloads, default declarations, and nested
`import()` types dependent on formatting. In development the declaration callback was invoked twice
for one generation, so the identity and final artifact could observe different source revisions.
Remote declaration bundles are untrusted control-plane input and delimiter balancing is not a
sufficient executable-syntax boundary.

## Decision

1. `wake_ecma_parser` owns declaration syntax facts. A TypeScript declaration parse returns exact
   item spans, declaration kinds, export/default/ambient-modifier facts, forbidden public-type
   `any` spans, cooked module requests with their complete quoted-literal spans and typed roles, and
   import-usage facts that distinguish type-only, declaration-referenced value, and runtime
   side-effect edges. Declaration references retain separate type and value namespaces: generic,
   conditional `infer`, and mapped-key type bindings suppress only type-position references within
   their exact lexical lifetime, while function, method, and arrow signature parameter bindings
   suppress `typeof` references across the complete signature and generic type parameters never
   suppress value imports. A whole
   type-parameter list is one lexical scope, including forward references in constraints; type-only
   and inline-type import bindings use the same reference filter, and an `export type` alias records
   only its local name rather than its public alias.
   Standalone and already-ambient templates retain independent request ranges. Speculative parses
   roll these facts back with the parser checkpoint that produced them.
2. `wake_tsdoc` owns the frozen declaration graph. It reads and resolves the graph once, retains
   parser facts beside owned source/declaration text, and exposes typed operations for declaration
   rendering, module-request rebinding, parser-proven ambient-body rendering, and strict
   declaration-body validation. Its injectable declaration filesystem freezes canonicalization,
   file probes, and reads against the product generation. Product crates do not recreate lexical or
   syntactic scanners.
3. Module-request rewriting replaces only parser-proven quoted module literals, preserving their
   original quote style and escaping the replacement. User comments, ordinary
   strings, string literal types, and text that happens to contain a reserved Federation namespace
   are never changed by identity binding.
4. A Federation declaration generation has build-independent canonical identity bytes and a pure
   `BuildId -> FederationTypeOutput` binder over the frozen graph. Production and development both
   prepare once per candidate. The development server treats this generation as opaque and never
   reparses JSON or removes a BuildId from serialized bytes.
5. Remote bundles retain the v1 JSON wire format, but every module key must be either an exact public
   expose specifier or a non-empty member of that build's reserved source namespace. Every module
   body must pass parser-owned declaration-only validation and the public-`any` policy before any
   stable editor index is published.
6. Dependency direction is `wake_app -> wake_tsdoc -> wake_ecma_parser -> lexer/ast/common`.
   `wake_dev_server` receives only an opaque prepared generation callback and does not depend on the
   declaration parser or renderer.

## Invariants

- One product candidate canonicalizes, resolves, reads, and parses each declaration input through
  one injected generation filesystem; shared multi-entry dependencies are read once.
- Build identity calculation and final type artifact rendering consume the same frozen graph.
- No declaration product code infers module requests, declaration kinds, ambient modifiers, or
  forbidden public types from generated text.
- Runtime-only imports do not enter declaration output or reachability. A declaration-file
  side-effect edge, including a zero-binding `import type {}`, is retained only when the frozen
  graph resolves it to another supported TypeScript declaration source.
- Required local declaration edges skip existing runtime/resource literals and resolve only to a
  supported TypeScript declaration source; a colocated `.js`, `.mjs`, `.cjs`, or resource file
  cannot shadow a later TypeScript declaration candidate.
- Rebinding cannot alter bytes outside parser-proven module-request ranges or typed module keys.
- Invalid, executable, foreign-namespace, explicit public-`any`, or implicit public-`any` remote
  declaration bodies fail before publication; the last-good stable index remains current.
- The v1 Federation type bundle remains deterministic and build-bound.

## Evidence

- `crates/wake_ecma_parser` publishes declaration facts and strict declaration parsing.
- `crates/wake_tsdoc` resolves, freezes, renders standalone/ambient templates, and rewrites
  declaration graphs from those facts without filesystem access during rendering.
- `crates/wake_app/src/federation_types.rs` packages and binds the frozen graph without scanning
  declaration strings.
- `crates/wake_app/src/federation_type_sync.rs` validates remote module ownership and declaration
  syntax before staging editor files.
- `crates/wake_dev_server/src/federation.rs` prepares one opaque `FederationTypeGeneration`, hashes
  its canonical identity, and invokes its pure binder once.
- `scripts/check-architecture.test.mjs` rejects the retired declaration scanners and whole-body
  Federation identity replacement.

## Consequences

Declaration facts retain source ranges and owned text until a candidate is materialized. This costs
bounded memory proportional to the declaration closure, but removes a second source traversal and
makes output independent of whitespace and line layout. The remote wire remains source text for v1,
so hosts still parse untrusted declarations once; a future structured wire format can reuse the same
validation and graph boundary. Explicit `.mjs` and `.cjs` declaration edges prefer their NodeNext
`.mts`/`.d.mts` and `.cts`/`.d.cts` source twins, then compatible generic TypeScript candidates.
Declaration resolution deliberately skips an existing runtime literal because it cannot contribute
parser-owned declaration facts; the separate component-document resolver retains its existing
literal-file behavior.

ADR 0028 的 generation ownership 与 single-read invariant 保持不变；canonical identity 现在是
first-class frozen-graph projection，而不是 placeholder JSON rewrite。

## Validation

- Parser tests cover same-line exports, classes, overloads, declaration containers, malformed
  delimiters, balanced-but-invalid interface/type members, missing or holed generic parameters and
  arguments, context-invalid `const` parameters, optional index signatures, mixed mapped members,
  parameter-property modifiers, multiline requests, `import = require`, structured `import()`
  types, type/value namespace separation, exact generic-list/`infer`/mapped-key/signature scope,
  type-import and type-export alias selection, speculative rollback, explicit and implicit public
  `any`, and strict executable-syntax handling.
- Declaration-renderer tests prove that only request spans are rewritten and user strings/comments
  containing reserved namespace text remain byte-identical. Counting filesystem tests prove that
  shared multi-entry sources are read once, runtime asset imports are not read, valid declaration
  augmentations remain reachable, NodeNext `.mjs`/`.cjs` requests choose format-matched TypeScript
  sources before compatibility fallbacks, existing runtime literals cannot shadow those fallbacks,
  and repeated standalone/ambient renders perform no I/O.
- Federation tests cover one prepare/bind per generation, build-independent identity, adversarial
  placeholder text, foreign module namespaces, executable, balanced-but-invalid and otherwise
  invalid remote bodies, and last-good publication.
- `cargo test -p wake_ecma_parser`
- `cargo test -p wake_tsdoc`
- `cargo test -p wake_app --lib`
- `cargo test -p wake_dev_server`
- `cargo clippy -p wake_app --all-targets -- -D warnings`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`
- `git diff --check`

## Supersedes

None.

## Amends

- [ADR 0028](0028-build-generation-ownership-and-observation-cache.md): decision 5 的 placeholder rendering 改为 parser-owned frozen declaration graph

## Removal plan

Remove declaration-oriented regular expressions, contextual string scanners, delimiter validators,
and whole-body namespace replacement in the same migration. No compatibility scanner or fallback
path remains in product code.
