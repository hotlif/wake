# ADR 0013: Add a native component-library product boundary

- Status: proposed
- Date: 2026-08-18

## Context

Crab component packages currently use Packify for four distinct jobs: library JavaScript and CSS,
TypeScript declarations, design-token source generation, and react-docgen JSON. Wake's application
build owns browser HTML, manifests and an IIFE runtime. The separate `wake bundle` contract from
ADR 0008 owns exact-file browser IIFE and Node CommonJS bundles, but it has no true ESM renderer,
declaration graph, token generator or react-docgen-compatible output.

The existing bundler link plan is not a format-neutral library IR: each module is converted to a
CommonJS body before the plan records tree-shaking and chunk placement. Rewrapping that output in an
ES module would preserve a Wake runtime rather than create a real library module, and therefore is
not an acceptable compatibility implementation.

## Decision

Wake will add a separate `library` product boundary. `wake_app` owns the public operation contracts,
project resolution, cancellation and transactional artifact writes. `wake_bundler` will own a new
format-neutral symbol link plan and the ESM/CommonJS renderers. A type-syntax graph shared by
declaration and docgen generation will remain independent from the runtime AST, which intentionally
erases TypeScript types.

The public commands will be `wake library build`, `wake library token` and `wake library docgen`.
The npm API will expose `buildLibrary`, `generateCssToken` and `generateDocgen`. A capability is not
published until its native implementation and semantic tests exist; in particular, library build
must not be implemented as ESM wrapping a CommonJS or IIFE bundle.

Library JavaScript treats every bare package and Node builtin as a runtime external. An external
edge may additionally have an analysis target used by static CSS evaluation; analysis targets never
enter runtime chunks. CSS compilation uses `@crab-dev/css` and statically provable values. Existing
Linaria packages migrate source-by-source rather than adding arbitrary JavaScript evaluation to the
compiler.

Token generation is the first independent vertical slice. It reads `token.toml` through Wake's
PnP-aware filesystem, follows package imports recursively, validates every `$ref`, detects cycles,
escapes generated TypeScript and atomically replaces only the configured output file.

## Invariants

- Existing application build and ADR 0008 bundle behavior remain unchanged.
- ESM library output contains no Wake IIFE, module table or browser loader.
- Bare runtime dependencies are never bundled, including dependencies loaded for static analysis.
- CSS static-analysis failure is an error; styles are never silently omitted.
- Library output is staged and exchanged transactionally; failure preserves the last valid output.
- Type declaration generation never substitutes `any` for an unsupported public inference.
- CLI, Node-API and npm entry points converge through `wake_app`.
- Generator output contains no absolute checkout path or nondeterministic metadata.

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_ecma_codegen/src/lib.rs`
- `crates/wake_ecma_parser/src/stmt.rs`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- `toolbox/packify/src/index.ts` in crab-dev
- `toolbox/packify/src/generateCssToken.ts` in crab-dev

## Consequences

The work is a compiler extension rather than a CLI alias. The link IR, declaration graph and
repository migration are separately testable milestones. React Compiler 19 memoization and general
WYW execution are outside the first native contract; React runtime semantics and Crab's actual
static CSS forms remain required.

## Validation

- Run token text-golden, recursive-import, cycle, missing-reference, PnP and atomic-write tests.
- Run ESM live-binding/cycle/re-export/TLA and CommonJS interop tests against Node.
- Run CSS cross-package static-value and stable-class-name fixtures.
- Typecheck generated declarations from a real consumer project.
- Compare docgen JSON structurally against the Packify oracle.
- Run the architecture gate and all existing application, bundle, Node API and npm regressions.
- Build every Crab component and verify tarballs before removing Packify.

### Implementation checkpoint (2026-08-18)

The first native vertical path is implemented: format-neutral runtime/analysis edges, true
preserve-module ESM and CommonJS renderers, strict static CSS extraction, stable package-prefixed
class names, a source-preserving declaration module graph, and staging/backup exchange for the four
Packify output directories. Rust CLI and Node/npm expose build, token and docgen through `wake_app`.

An isolated copy of the current crab-dev component workspace built 43 of 51 package entries without
source changes. The remaining eight failures are intentionally fail-closed: four public declaration
inference forms and four unsupported complex TSX parser forms. The declaration graph follows public
re-exports and type edges rather than implementation-only runtime imports, preventing internal
constants from becoming false public-type failures.
Compound-extension declaration imports and workspace analysis links are covered by regression/probe
fixtures; the remaining categories are migration gates. Representative
`rc-alert`, `rc-divider`, and `rc-button` builds pass when their workspace analysis dependency is
available, and every emitted declaration in those canaries passes the real TypeScript syntax gate.

The declaration output currently preserves internal modules under `declarations/_wake` behind the
stable `declarations/index.d.ts` entry. Symbol flattening, full consumer typechecking across all 51
packages, magic/legal comment preservation, tarball verification, and two canary cycles remain
required before Packify retirement.

## Supersedes

None.

## Removal plan

Keep Packify as an explicit deprecated shim until the native build, token and docgen contracts pass
two canary release cycles. Then migrate package scripts and dependencies, remove Packify-specific
Turbo ordering, and delete the shim. There is no silent fallback to the Node implementation.
