# ADR 0015: Scope automatic completion triggers to embedded CSS

- Status: accepted
- Date: 2026-08-18

## Context

`wake_css_language` already computes property, value, at-rule and pseudo-selector completions for
recognized `@crab-dev/css` templates. The VS Code Extension Host test requested those items through
`vscode.executeCompletionItemProvider` without a trigger character, which is equivalent to an
explicit completion request and did not prove the normal typing experience.

The language server advertised only `:`, `@` and `-` as completion triggers. Embedded CSS is hosted
inside JavaScript and TypeScript template-string tokens, where VS Code does not use ordinary code
quick suggestions for a property letter. Consequently, typing `d` in a Crab CSS template did not
open the existing `display` completion automatically.

Advertising letters as LSP trigger characters does not solve this gap. VS Code deliberately skips
trigger-character providers when the cursor is in a word that would normally use quick suggestions,
then suppresses that quick-suggestion request for a string token. Letter triggers are also scoped to
the whole TypeScript document rather than to an embedded language region.

## Decision

After applying an incremental identifier insertion, `wake_css_lsp` asks the updated
`LanguageDocument` whether the resulting cursor has at least one Crab CSS completion. Only then does
the server emit `crabCss/triggerSuggest` with the document URI, version and cursor position. The
client invokes `editor.action.triggerSuggest` only when all three still match the active editor.

The standard LSP completion triggers remain `:`, `@` and `-`. They handle punctuation directly and
do not claim ordinary letters across an entire JavaScript or TypeScript document.

`wake_css_language::LanguageDocument::completions` returns `None` when the host position is outside
a semantically recognized Crab CSS virtual document or inside an interpolation hole. The LSP
transports that as no response from this provider. Inside a recognized template it returns a list,
including an empty list when the provider applies but no item matches.

The extension does not override `editor.quickSuggestions.strings`, because that setting affects
ordinary strings and every completion provider in JavaScript and TypeScript. The client owns only
stale-notification rejection and the editor command; it does not recognize templates or decide
whether completion applies.

When VS Code applies a property item such as `display: `, the resulting incremental replacement is
handled by the same post-analysis notification path as identifier typing. The server recognizes the
bounded `property: ` edit shape, analyzes the new document version and emits the notification only
when the new value position has semantic candidates. This avoids querying the server before the
completion edit has synchronized. The language layer returns only values declared for the current
property and filters them by the value prefix already typed.

The LSP assigns stable `sortText` keys from the semantic fact order. VS Code therefore keeps common
standard values ahead of legacy vendor values instead of alphabetically promoting `-moz-*` items.

## Invariants

- Semantic binding analysis is the only authority for deciding whether a host position is Crab CSS.
- `:`, `@` and `-` remain triggers for values, at-rules and prefixed properties.
- Automatic suggestions require a positive result from the updated server analysis.
- Stale document versions, moved cursors and stopped or replaced clients cannot trigger suggestions.
- Positions outside virtual CSS documents return no Crab CSS completion response.
- The extension never enables suggestions globally for JavaScript or TypeScript string tokens.
- The VS Code client owns no source or template recognizer.
- Property-item replacement requests follow-up values only after the updated server analysis.
- Property-value candidates are scoped to the current property and filtered by their typed prefix.
- Completion ranking preserves the deterministic semantic fact order.
- Minification must preserve evaluation of every `void` operand; the client intentionally uses
  `void vscode.commands.executeCommand(...)` for fire-and-forget editor commands.

## Evidence

- `crates/wake_css_language/src/lib.rs`
- `crates/wake_css_language/src/tests.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `crates/wake_css_lsp/src/main.rs`
- `editors/vscode-css/src/extension.ts`
- `editors/vscode-css/test/manifest.test.mjs`
- `editors/vscode-css/test/suite/index.ts`
- `editors/vscode-css/README.md`
- `crates/wake_ecma_minify/src/const_eval.rs`
- `crates/wake_bundler/src/tests.rs`

## Consequences

Typing a CSS property prefix opens the existing suggestions inside recognized Crab CSS templates
even though the host token is a template string. The server sends a small notification only for
Crab positions that have candidates; ordinary JavaScript and TypeScript edits cause no additional
request or notification. Ordinary string suggestions and TypeScript navigation behavior remain
unchanged.

The Wake constant evaluator reports `void expression` as foldable only when `expression` itself is
constant. This prevents minification from replacing `void sideEffect()` with `undefined` and is a
general bundler correctness rule, not an editor-specific workaround.

## Validation

- Test property, value, at-rule and pseudo completions inside templates and `None` outside them.
- Type a property prefix one character at a time, accept the automatically opened suggestion and
  assert the edit in a real VS Code Extension Host.
- Keep the standard LSP punctuation trigger set covered by a capability test.
- Bundle a `void`-wrapped command call with minification and assert that its argument remains in the
  output; inspect the compiled extension for `editor.action.triggerSuggest`.
- Run affected Rust tests, Clippy, extension checks, architecture checks and VSIX inspection.

## Supersedes

None. This decision completes the editor interaction contract established by
[ADR 0007](0007-css-language-intelligence.md) while preserving the shared syntax and semantic
binding boundaries in [ADR 0010](0010-shared-css-syntax-tree.md).

## Removal plan

Replace the explicit-only Extension Host assertion and the timer-driven `hasCompletion` request in
the same change. Do not retain alphabetic LSP triggers, a global quick-suggestions override or a
client-side recognizer as a fallback.
