# ADR 0010: Own all CSS syntax in wake_css

- Status: accepted
- Date: 2026-08-17

## Context

Wake has three CSS consumers: ordinary stylesheet bundling in `wake_css`, build-time templates in
`wake_css_in_js`, and editor intelligence in `wake_css_language`. They previously made structural
decisions with independent byte scanners or a language-only parser. Those implementations could
disagree on comments, quoted text, escapes, nested functions, at-rules and declaration boundaries.
Fixing semantic highlighting alone did not prevent compiler and bundler paths from retaining a
second interpretation of CSS.

The repository already uses parser and semantic AST identity for JavaScript/TypeScript template
discovery. CSS needs the same single-authority property below every CSS consumer.

## Decision

`wake_css` owns the public `syntax::CssSyntaxTree` concrete syntax tree, including decoded token
kinds, nested block structure, grammar-context items, declarations, syntax errors, source spans,
token payload spans and token serialization categories. Callers select an explicit `Stylesheet`,
`StyleBlock`, `Keyframes` or `ComponentValues` entry context. It is the only CSS syntax authority in
the workspace.

`wake_css` uses that tree for imports, URLs, minification and CSS Modules. `wake_css_in_js` uses it
for nesting, selectors, keyframes, animation references, scoped at-rule checks, URL checks and
declaration removal. `wake_css_language` builds editor features from the same tree. Consumers may
apply domain rules to decoded nodes and may edit text through parser-owned spans; they must not add
regular-expression, spelling-search or byte-scanner fallbacks for CSS structure.

Each immutable CSS text snapshot is parsed once per consumer operation and the resulting tree is
reused across its syntax-sensitive decisions. If a transformation creates different CSS text, that
new snapshot may be parsed as a new unit. JavaScript/TypeScript template discovery remains owned by
the ECMA parser and semantic binding analysis.

## Invariants

- `wake_css` is below compiler, language-service and product crates and owns no product behavior.
- Comments and strings cannot activate imports, URLs, global escapes, at-rules or selectors.
- Escaped CSS identifiers are interpreted from decoded parser tokens, not source spelling.
- Nested functions and blocks are traversed through child nodes, not delimiter counting.
- Rules and declarations are distinguished by shared context items, not by treating every curly
  block or `ident:` pair identically.
- Comment removal uses parser token serialization categories and cannot merge adjacent tokens.
- Rewrites use parser-owned source spans and preserve untouched source bytes.
- Compiler and editor template discovery uses ECMA AST plus semantic binding identity only.
- No TextMate grammar, regex recognizer or manual byte scanner is a compatibility fallback.

## Evidence

- `crates/wake_css/src/syntax.rs`
- `crates/wake_css/src/lib.rs`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_in_js/src/nesting.rs`
- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `editors/vscode-css/test/manifest.test.mjs`
- `engineering/architecture-boundaries.json`

## Consequences

CSS behavior now has one dependency direction and one escape/structure interpretation across build
and editor paths. Parser improvements benefit every consumer, and tests can exercise escaped syntax
that spelling scanners cannot recognize. `wake_css` exposes a larger stable internal API and every
CSS operation pays for a CST allocation; consumers avoid duplicate parses within an operation to
bound that cost.

Adding syntax support now requires extending the shared node model rather than adding a local scan.
Domain-only text processing such as URL scheme policy, generated-name sanitization and output
serialization remains allowed after a parser node has established the syntactic boundary.

## Validation

- Run tests for `wake_css`, `wake_css_in_js`, `wake_css_language` and `wake_css_lsp`.
- Run focused bundler CSS tests and Clippy with warnings denied.
- Test strings/comments as negative lookalikes and escaped identifiers as positive parser cases.
- Check the VS Code package, the architecture policy, formatting and `git diff --check`.
- Search affected runtime sources for removed scanner/regex entry points; test-only assertions and
  non-syntax domain matching are not alternate CSS parsers.

## Supersedes

[ADR 0009](0009-semantic-css-highlighting.md). Its semantic-binding-only highlighting decision and
removal of the TextMate fallback remain part of this broader shared-syntax decision.

## Removal plan

Delete the language-owned syntax-tree module and all CSS import, URL, nesting, at-rule, declaration
and class-selector scanners in the same migration. Do not retain compatibility wrappers that parse
structure independently; the only permitted wrapper accepts CSS text and immediately constructs the
shared tree.
