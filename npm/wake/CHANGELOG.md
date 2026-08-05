# Changelog

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
