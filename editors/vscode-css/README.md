# Crab CSS for VS Code

Crab CSS adds CSS highlighting, completion, hover, diagnostics, colors, symbols, folding, selection
ranges and explicit formatting to `css`, `keyframes` and `globalStyle` templates imported from
`@crab-dev/css`.

The extension uses a native Rust language server. Canonical tag names receive immediate TextMate
highlighting; semantic analysis recognizes import aliases and ignores shadowed local bindings.

## Compatibility

- VS Code 1.96 or newer
- `@crab-dev/css >=0.1.0 <0.2.0`

The extension never executes project JavaScript. TypeScript definition, reference and rename
features remain owned by VS Code's built-in TypeScript service.
