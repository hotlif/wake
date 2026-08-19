# Changelog

## 0.1.2

- Keep CSS identifier values visually independent from TypeScript keywords through the
  theme-aware `crabCssValue` semantic token.
- Automatically show property completions while typing inside recognized Crab CSS templates
  without enabling suggestions for ordinary TypeScript strings.
- Automatically open property-specific value suggestions after accepting a CSS property, filter
  them by the typed value prefix, and keep common standards ahead of legacy vendor values.

## 0.1.1

- Replaced regex/TextMate highlighting with AST and semantic-token-based CSS intelligence.
- Added compiler-accurate diagnostics and formatting backed by the shared CSS syntax tree.
- Added five-platform VSIX packaging and automatic GitHub Release publication without Marketplace publishing.

## 0.1.0

- Add AST, semantic-binding and CSS-syntax-tree-aware highlighting and native language intelligence.
- Add on-type and compiler-accurate on-save diagnostics.
- Add explicit document and range formatting with interpolation-safe edits.
