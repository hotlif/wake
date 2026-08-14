# ADR 0003: Make compiler stages and shell dependencies explicit

- Status: accepted
- Date: 2026-08-14

## Context

Wake documents `wake_app` as the shared owner of CLI and Node build behavior, but the executable
boundary policy only prevented internal crates from depending on the two shells. A shell could
therefore depend directly on `wake_bundler` and still pass the architecture check. The compiler
group also had no internal dependency constraints.

`wake_ecma_parser` additionally re-exported `wake_ecma_semantic` as an open-ended compatibility
path. At the same time, context-sensitive browser lowering is intentionally coordinated while the
parser still owns cover grammar and lexical scope information.

## Decision

Make the compiler dependency graph executable. `wake_common` is the workspace foundation;
`wake_ecma_ast` and `wake_ecma_lexer` consume only the foundation; `wake_ecma_semantic` and
`wake_ecma_transform` consume only common AST models; and `wake_ecma_parser` may consume lexer plus
transform lowering primitives, but not Semantic.

Remove the Semantic re-export from `wake_ecma_parser`. Callers that perform analysis depend on
`wake_ecma_semantic` directly. Treat parse-time lowering as a deliberate fused front-end operation:
the parser owns syntax context and the transform crate owns reusable lowering rules and AST
construction helpers.

Allow CLI and Node shells to depend directly on compiler crates for their explicit compiler-facing
commands and experimental APIs. Require all build, configuration, server, Docs and lifecycle
behavior to enter through `wake_app`; shells may not depend directly on orchestration or other
product crates.

## Invariants

- `wake_common` has no workspace dependency.
- Parser does not own or re-export Semantic analysis.
- Transform helpers do not depend on Parser, orchestration or products.
- CLI and Node build behavior reaches the build stack through `wake_app`.
- Experimental compiler APIs cannot create a second bundler, resolver, cache or server path.
- Every rule above has a failing architecture-check fixture.

## Evidence

- `crates/wake_ecma_parser/Cargo.toml`
- `crates/wake_ecma_parser/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_cli/src/main.rs`
- `engineering/architecture-boundaries.json`
- `scripts/check-architecture.test.mjs`

## Consequences

Compiler consumers declare their actual owner instead of relying on the Parser façade. The
architecture policy becomes more restrictive and a future shell feature must choose explicitly
between `wake_app` behavior and compiler-only behavior. Context-sensitive lowering remains fused
with parsing, so a browser-target change can invalidate the front-end task; this is an intentional
correctness tradeoff rather than an undocumented phase inversion.

## Validation

- Run `npm run architecture:test` and `npm run architecture:check`.
- Run Parser, Semantic, Bundler, CLI and Node checks/tests.
- Verify fixtures reject `wake_common -> wake_css`, `wake_ecma_parser -> wake_ecma_semantic` and
  `wake_cli -> wake_bundler`.
- Run `cargo metadata --no-deps` and inspect the resulting workspace dependency graph.

## Supersedes

None.

## Removal plan

The Parser Semantic façade and dependency are removed atomically; no compatibility wrapper remains.
If parse-time lowering is later separated, a new ADR must replace this decision and preserve cover
grammar, scope-temporary and source-map correctness before changing the dependency direction.
