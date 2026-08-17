# Changelog

## 0.1.17

- Added AST-driven CSS parsing shared by compilation, CSS-in-JS nesting, editor diagnostics, and semantic highlighting, removing the legacy TextMate injection grammar.
- Added Node library bundle APIs and CLI support with incremental rebuild coverage and updated documentation.
- Added complete CI release coverage for all seven public npm packages and GitHub-only multi-platform VSIX releases.

## 0.1.16

- Added Node 20 CommonJS single-file bundles with atomic exact-outfile writes, host externals, declaration-ordered Node conditional exports, usable source-map results, and matching Rust/npm CLI support.
- Automatically loaded Crab UI component CSS from package identity across `node_modules`, workspaces, and Yarn PnP virtual, unplugged, and zip layouts without runtime CSS imports.
- Added issuer-scoped Yarn PnP dependency fallbacks for Components workbenches while preserving aliases, package-owned dependency versions, exports errors, and ambiguous virtual-locator rejection.
- Preserved configured Demo accent colors independently from the neutral workbench theme.
- Added a Yarn 4.16 Plug'n'Play release gate that packs all six local npm packages and verifies the Components runtime, hashed CSS link, and direct and transitive component styles.
- Preserved ESM default and re-exported bindings in minified code-split builds while retaining correct CommonJS default and namespace interop.
- Made dev-server startup wait for file-watcher registration and canonicalized project paths, eliminating missed immediate edits across Windows short/long path aliases while surfacing watcher initialization failures to API callers.

## 0.1.15

- Preserved explicitly unset component Props, non-default Args, selected demos, and viewport state across copied URLs, refreshes, and browser navigation.
- Recovered component previews after control changes or resets and reported React render errors and unhandled promise rejections in the workbench.
- Prevented the Windows CLI and Node-API library from racing to write the same debug PDB during workspace builds.
- Added complete stable and experimental Node.js API references, restored versioned engineering documentation, and enforced documentation routes and links in CI.

## 0.1.14

- Rebuilt the Components workbench with published Crab UI controls, responsive drawers, compact toolbars, source dialogs, and mobile-first layouts.
- Changed the default documentation locale to Simplified Chinese while preserving explicit English configuration.
- Updated the React documentation fixture to consume the published Crab Button package and use Chinese-first content.

## 0.1.13

- Preserved complete function parameter lists during production minification, preventing undefined bindings in Motion and Framer Motion bundles.

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
