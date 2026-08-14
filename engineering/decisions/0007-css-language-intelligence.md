# ADR 0007: Own CSS language intelligence below editor products

- Status: accepted
- Date: 2026-08-14

## Context

`@crab-dev/css` already has one build-time semantic owner in `wake_css_in_js`, but Wake has no
editor language service. TextMate grammars can color common tag spellings, yet cannot prove that an
alias refers to an import from `@crab-dev/css` or distinguish a shadowing local binding. Putting an
editor session, workspace files, or LSP protocol into the compiler or bundler would also reverse the
existing dependency direction.

## Decision

`wake_css_language` owns file-system-independent discovery of Crab CSS tagged templates, virtual CSS
documents, host-to-virtual source mapping and CSS editing analysis. It consumes the existing parser,
semantic model and `wake_css_in_js` contract, but does not depend on the bundler, resolver, LSP or an
editor.

`wake_css_lsp` is a product edge. It owns LSP transport, document versions, bounded caches,
workspace resolution and dependency-aware saved-document analysis. Exact Crab compiler diagnostics
come from `wake_css_in_js`; the language service does not copy its static evaluation rules.

`editors/vscode-css` is a thin workspace extension. It contributes declarative TextMate highlighting
and launches the target-specific Rust server. TypeScript continues to own JavaScript/TypeScript
definition, reference and rename behavior.

## Invariants

- Only semantic bindings imported from `@crab-dev/css` activate Crab CSS behavior.
- `wake_css_language` has no file-system, resolver, bundler, editor or protocol ownership.
- Build-time static evaluation and `CRAB_CSS_*` diagnostics remain owned by `wake_css_in_js`.
- Host edits never cross or modify a `${...}` interpolation.
- LSP positions use zero-based UTF-16 units and retain the host document identity.
- Results computed for an older document version are never published for a newer version.
- Runtime caches are bounded and cache identity contains document version, configuration and saved
  dependency inputs.
- Every shipped VSIX contains exactly one platform server binary.

## Evidence

- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_common/src/source.rs`
- `engineering/CRAB_CSS.md`
- `engineering/CSS_LANGUAGE_SERVICE.md`
- `engineering/architecture-boundaries.json`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/test/`
- `.github/workflows/vscode-css.yml`

## Consequences

Wake gains a reusable CSS analysis layer and a native editor product without making the bundler an
editor dependency. The Rust server and generated CSS fact data increase build and release surface.
TextMate provides immediate coloring for canonical spellings while semantic tokens provide precise
alias-aware coloring after analysis.

The extension version is independent from Wake. Version `0.1.x` supports VS Code 1.96 or newer and
`@crab-dev/css >=0.1.0 <0.2.0`; unsupported package versions keep syntax coloring but disable exact
compiler diagnostics with an actionable warning.

## Validation

- Run `npm run architecture:check` after every crate-boundary change.
- Test alias, shadowing, incomplete syntax, interpolation mapping, CRLF and UTF-16 positions in
  `wake_css_language`.
- Test LSP initialization, document lifecycle, stale-result suppression and protocol features in
  `wake_css_lsp`.
- Build and inspect one VSIX per supported target in the release matrix.
- Run workspace tests, Clippy, editor package checks and `git diff --check`.

## Supersedes

None.

## Removal plan

No earlier language server exists. Experiments or temporary parsers used to validate recovery must
be removed before this decision is accepted; no compatibility crate, old editor directory or second
CSS package specifier may remain.
