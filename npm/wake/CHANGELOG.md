# Changelog

## 0.1.12

- Flattened installed dependencies by npm name, version, and package subpath while preserving distinct versions, peer contexts, Yarn PnP virtual instances, and local workspaces.
- Preserved CommonJS and cyclic-module factory lifecycles and resolved the React jsxDEV runtime lazily to prevent cycle-captured exports.

## 0.1.11

- Added production compatibility for dependencies published with React jsxDEV, including Yarn Plug'n'Play package layouts.
- Documented and verified relative public_path = "./" assets for Electron and file:// builds.

## 0.1.10

- Preserved mangled names for exports declared through object and array destructuring.
- Fixed `cancelFrame is not defined` in bundled Motion dependencies.

## 0.1.9

- Added webpack-style runtime module and cache namespaces.
- Prevented module-factory parameter collisions with imported bindings.
- Stabilized large generated modules against linker, tree-shaking, and mangle divergence.

## 0.1.8

- Prevented empty hoisted registry bodies from emitting invalid comma-only JavaScript expressions.
- Restored reproducible release builds by locking available compatible transitive dependencies.

## 0.1.7

- Prevented empty hoisted registry bodies from emitting invalid comma-only JavaScript expressions.

## 0.1.6

- Lowered import.meta.hot and import.meta.url for classic-script chunks.
- Preserved explicit keys in nested object destructuring patterns.
- Preserved required parentheses when nullish coalescing is mixed with logical operators.
- Prevented mangled declarations from colliding with preserved import aliases.
- Parenthesized retained object and anonymous-class initializers after tree shaking.

## 0.1.5

- Stabilized Windows incremental rebuild events by coalescing delayed file notifications.

## 0.1.4

- Added the opt-in Storybook-like documentation component workbench.
- Fixed Yarn PnP packages that expose entries and subpaths through `package.json#exports`.
- Expanded TypeScript and TSX parsing for optional arrow parameters, generic JSX, generic async arrows, and indexed type queries.
- Added source module paths to build diagnostics and corrected `wake parse` source-type selection.

## 0.1.3

- Improved the npm development-server terminal panel with Rust CLI-aligned styling, startup timing, rebuild progress, diagnostics, and color controls.

## 0.1.2

- Updated the npm release pipeline for private GitHub repositories while preserving immutable tarball audits.

## 0.1.1

- Fixed cross-platform output paths and source-map source names.
- Fixed concurrent documentation generation atomic-write races on Windows.
- Stabilized Windows native builds by pinning the NAPI-RS CLI.
- Aligned CI coverage with the supported Node.js 24 and 26 releases.

## 0.1.0

- First Wake npm release.
- Added native build, bundle, build-context, dev-server, and documentation APIs.
- Added experimental tokenize, parse, transform, and semantic-analysis APIs.

The experimental subpath may change during the 0.x release series.
