# Crab CSS for VS Code

Crab CSS adds CSS highlighting, completion, hover, diagnostics, colors, symbols, folding, selection
ranges and explicit formatting to `css`, `keyframes` and `globalStyle` templates imported from
`@crab-dev/css`.

The extension uses a native Rust language server. Highlighting is driven exclusively by AST and
semantic binding analysis, so import aliases work while shadowed bindings and same-named tags from
other packages are ignored. The client does not inspect source text or dependency manifests to
guess whether a document contains Crab CSS. Discovered templates are parsed once into the CSS
concrete syntax tree owned by `wake_css`, shared with compiler and bundler syntax consumers and
reused by highlighting, diagnostics, hover, colors, folding and formatting.

## Compatibility

- VS Code 1.96 or newer
- `@crab-dev/css >=0.1.0 <0.2.0`

The extension never executes project JavaScript. TypeScript definition, reference and rename
features remain owned by VS Code's built-in TypeScript service.

Property suggestions open automatically from the first typed property letter inside a recognized
Crab CSS template. `:`, `@` and `-` continue to trigger value, at-rule and prefixed-property
suggestions. Manual completion (`Ctrl+Space` / `Cmd+Space`) remains available. The extension does
not enable string suggestions globally, so ordinary JavaScript and TypeScript strings are
unaffected.

## Theming

CSS identifier values use the custom semantic token `crabCssValue` instead of the host language's
`keyword` token. Themes without an explicit semantic rule fall back to the standard
`support.constant.property-value.css` TextMate scope. This keeps values such as `inline-flex`,
`center` and `unset` visually distinct from TypeScript keywords without hard-coding a color.

Users can override the value style independently:

```json
{
  "editor.semanticTokenColorCustomizations": {
    "rules": {
      "crabCssValue": "#D19A66"
    }
  }
}
```
