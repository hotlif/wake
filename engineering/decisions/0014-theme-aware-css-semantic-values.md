# ADR 0014: Give embedded CSS values a theme-aware semantic identity

- Status: accepted
- Date: 2026-08-18

## Context

`wake_css_language` previously classified every CSS identifier that was not a declaration name as
the standard semantic token `keyword`. In TypeScript and TSX documents, VS Code therefore applied
the same theme rule to CSS values such as `inline-flex`, `center` and `unset` as it applied to host
language keywords such as `import`, `from` and `const`.

The shared `wake_css::syntax::CssSyntaxTree` already records exact declaration name and value spans,
so the language service can distinguish declaration values without another parser or spelling
heuristic. VS Code extensions can declare custom semantic token types and map them to established
TextMate scopes for themes that do not define an explicit semantic rule.

## Decision

`wake_css_language` classifies ordinary identifiers contained by a declaration's parser-owned
`value_span` as `SemanticKind::Value`. `wake_css_lsp` transports that kind as the custom semantic
token type `crabCssValue`. The VS Code extension manifest owns the public token declaration and maps
it to `support.constant.property-value.css` as the theme fallback.

`crabCssValue` has no `keyword` super type. The extension does not hard-code a foreground color.
Declaration names remain the standard `property` type, at-keywords remain `keyword`, and numbers,
strings and functions retain their standard semantic types. TypeScript expressions inside template
interpolations remain outside the virtual CSS document and are colored only by the host service.

## Invariants

- The shared CSS CST is the only authority for declaration name and value boundaries.
- CSS declaration values never acquire a host-language keyword identity merely because they are
  identifiers.
- Semantic tokens never cover TypeScript or JavaScript interpolation holes.
- The LSP legend, encoded token indexes and VS Code manifest use one stable token identifier.
- Themes own colors; the extension supplies a standard CSS TextMate fallback only.
- Semantic binding analysis remains the only authority for discovering Crab CSS templates.

## Evidence

- `crates/wake_css/src/syntax.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `editors/vscode-css/package.json`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/scripts/check-vsix.mjs`

## Consequences

CSS declaration values can be styled independently from TypeScript keywords while continuing to
follow each user's theme. Theme authors and users may target `crabCssValue` directly. Existing
themes that know standard CSS TextMate scopes gain an appropriate fallback without a Crab-specific
rule. The semantic legend gains one custom type, so clients must consume the legend instead of
assuming hard-coded indexes.

## Validation

- Test declaration names, nested declaration values, interpolation holes and non-declaration CSS
  identifiers in `wake_css_language`.
- Test the custom legend entry and encoded value token in `wake_css_lsp`.
- Test the manifest token declaration, absence of a keyword super type and CSS scope mapping.
- Run extension checks, affected Rust tests, Clippy, architecture checks, formatting and VSIX
  inspection.

## Supersedes

None. This decision refines the semantic-only highlighting path retained by
[ADR 0010](0010-shared-css-syntax-tree.md).

## Removal plan

Remove the old branch that classified declaration-value identifiers as `Keyword` in the same
change. No compatibility token, duplicate classifier or hard-coded theme color remains.
