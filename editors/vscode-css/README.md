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
