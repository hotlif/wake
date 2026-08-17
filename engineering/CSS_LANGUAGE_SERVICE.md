# Crab CSS language service

This document defines the protocol and ownership contract for editor intelligence around
`@crab-dev/css`. The durable architecture decision is
[ADR 0010](decisions/0010-shared-css-syntax-tree.md).

## Components

- `wake_css`: the shared CSS concrete syntax tree, decoded tokens, declarations, errors and source
  spans used by compiler, bundler and language consumers.
- `wake_css_language`: ECMA AST/semantic tag discovery, virtual CSS, source maps and reusable
  language features built from the shared CSS tree.
- `wake_css_lsp`: stdio protocol, document lifecycle, workspace resolution, saved dependency
  analysis and bounded caches.
- `editors/vscode-css`: configuration, commands and platform server launch; highlighting comes only
  from semantic tokens and has no TextMate fallback.

The compiler remains authoritative for static evaluation. Language features may add CSS syntax
diagnostics, but a diagnostic using a `CRAB_CSS_*` code must originate in `wake_css_in_js`.

## Document model

Every open document is identified by URI and monotonically increasing editor version. A parsed
snapshot contains discovered templates, virtual CSS text, source segments and one shared CSS CST.
The snapshot and tree are reused by all requests for that version.

Virtual contexts are deterministic:

- `css` is analyzed as a declaration and nesting block;
- `keyframes` is analyzed as keyframe steps;
- `globalStyle` is analyzed as a complete stylesheet.

Interpolation source is replaced with an equal-width sentinel in virtual text. Source maps expose
only literal template segments. Completion, formatting, code actions and other edits that touch an
interpolation or synthetic wrapper are discarded.

## Protocol surface

The server uses incremental text synchronization and supports semantic tokens, completion, hover,
publish diagnostics, document colors and presentations, document symbols, folding ranges,
selection ranges, document/range formatting and code actions. It intentionally omits
JavaScript/TypeScript definition, reference and rename providers.

On-type validation is debounced by 150 ms and analyzes only the current document. Saving performs
dependency-aware static export collection and compiler analysis for the saved module and known
reverse importers. Revision checks prevent stale results from being published. Closing or deleting a
document clears its diagnostics.

## Configuration

- `crabCss.enable`: boolean, default `true`.
- `crabCss.validation.mode`: `off`, `onType` or `onSave`; default `onType`.
- `crabCss.format.enable`: boolean, default `true`.
- `crabCss.trace.server`: `off`, `messages` or `verbose`; default `off`.

Formatting is explicit. The extension never changes `editor.formatOnSave`.

## Resource limits

Open documents remain resident. Closed dependency analysis uses an LRU capped at 512 modules or
128 MiB, whichever is reached first. Workspace traversal is lazy from open documents and never
starts with an unconditional project scan.

## Distribution

The extension is `crab-dev.crab-css`, display name `Crab CSS`, versioned independently from Wake.
Each VSIX contains one server for Windows x64, Linux x64, Linux arm64, macOS x64 or macOS arm64.
The extension runs as a workspace extension so remote workspaces use a server built for the remote
host.

## Validation commands

From the repository root:

```bash
cargo test -p wake_css_language -p wake_css_lsp
cargo clippy -p wake_css_language -p wake_css_lsp --all-targets -- -D warnings
npm ci --ignore-scripts --prefix editors/vscode-css
npm run vscode:css:check
```

`editors/vscode-css/scripts/update-css-data.mjs` deterministically regenerates the embedded fact
snapshot from the pinned `vscode-css-languageservice@6.3.10` package. CI regenerates the file and
requires a clean diff.
