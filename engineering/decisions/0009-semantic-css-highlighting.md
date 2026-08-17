# ADR 0009: Use semantic bindings as the only editor highlighting authority

- Status: superseded
- Date: 2026-08-17
- Superseded by: [ADR 0010](0010-shared-css-syntax-tree.md)

## Context

The VS Code extension registered a TextMate injection grammar that treated every tagged template
spelled `css`, `keyframes` or `globalStyle` as Crab CSS. TextMate can match spelling but cannot prove
the tag's import source, follow aliases or distinguish lexical shadowing. As a result, templates
imported from packages such as `@linaria/core` received incorrect Crab CSS highlighting.

The language stack already parses JavaScript and TypeScript into an AST. `wake_css_in_js` resolves
imports and references to semantic symbol identities, `wake_css_language` discovers only templates
bound to `@crab-dev/css`, and `wake_css_lsp` publishes semantic tokens for those template spans.
Within each discovered template, `wake_css_language` persists `cssparser` output as a concrete
syntax tree with nested blocks, decoded tokens, errors and virtual-document spans.

## Decision

AST parsing and semantic binding identity are the only authority for Crab CSS template discovery
and highlighting. `wake_css_in_js::discover_css_templates` owns recognition of supported bindings;
`wake_css_language` derives virtual CSS and tokens from those discoveries; `wake_css_lsp` transports
the tokens to editors.

CSS syntax-sensitive features consume one concrete syntax tree per virtual document. Semantic
tokens, diagnostics, hover, colors, completions, folding and formatting must not implement separate
regular-expression or byte-scanning recognizers.

Editor clients must not register spelling-based TextMate grammars or maintain another template
recognizer. They must not gate server startup by searching document text or dependency manifests
for package-name strings. The VS Code extension remains a thin launcher and configuration surface
for the native language server.

## Invariants

- Only semantic bindings imported from `@crab-dev/css` activate Crab CSS behavior.
- Import aliases are recognized, while lexical shadowing and same-named tags from other packages
  are ignored.
- One AST and semantic-binding path owns template discovery for compiler and editor consumers.
- One CSS concrete syntax tree owns syntax-sensitive language features inside each template.
- Semantic tokens retain host document spans and use zero-based UTF-16 LSP positions.
- Highlighting never requires executing project JavaScript.
- Client activation does not infer source semantics from text or manifest spelling.
- Every shipped VSIX contains exactly one target-specific language server and no Crab TextMate
  grammar.

## Evidence

- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/package.json`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/scripts/check-vsix.mjs`

## Consequences

Crab CSS highlighting follows semantic identity instead of tag spelling, eliminating interference
with Linaria and other CSS-in-JS packages while preserving aliases. Coloring begins after the
language server analyzes the document rather than immediately through TextMate. If the server is
unavailable, the extension does not guess and apply potentially incorrect Crab highlighting.

## Validation

- Test Crab import aliases, lexical shadowing and same-named imports from other packages in
  `wake_css_language`.
- Test semantic-token capability and UTF-16 encoding in `wake_css_lsp`.
- Check the extension manifest and packaged VSIX contain no Crab TextMate grammar.
- Run editor package checks, affected Rust tests, architecture checks and `git diff --check`.

## Supersedes

[ADR 0007](0007-css-language-intelligence.md)'s decision to combine TextMate coloring with semantic
tokens. Its language-service ownership and dependency direction remain valid.

## Superseded by

[ADR 0010](0010-shared-css-syntax-tree.md) moves CSS syntax ownership below the language service so
the compiler, bundler and editor consume the same CST while preserving semantic-only highlighting.

## Removal plan

Remove `syntaxes/crab-css.injection.json`, its manifest contribution, package allowlist entry and
packaging assertions in the same change. No compatibility grammar or second recognizer remains.
