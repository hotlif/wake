# ADR 0008: Separate Node library bundles from Web application builds

- Status: accepted
- Date: 2026-08-17

## Context

Wake's application build owns HTML, manifests, browser targets, CSS extraction and code splitting.
The VS Code CSS extension instead needs one Node 20 CommonJS file, host-provided `vscode` imports and
an exact output path. Its previous esbuild path duplicated TypeScript bundle ownership outside Wake.

## Decision

`wake_bundler` owns platform-aware dependency resolution, explicit external dependencies and entry
module format. `wake_app::bundle` owns the library contract and exact-file writes; Rust CLI, npm CLI
and Node-API bindings are shells over that service. Node dependency edges activate `node` plus the
edge's `import` or `require` condition, while the package author's declaration order selects the
first matching export. Node package fallbacks prefer `main` before `module`; browser fallbacks prefer
`module` before `main`. Web application build behavior remains separate and unchanged.

The first stable Node contract is a single CommonJS file. Node builtins are external automatically;
configured bare package names also externalize their subpaths. Bundle options are invocation inputs,
not `wake.config.toml` project configuration.

## Invariants

- Platform, format, target, external packages and dependency conditions participate in cache identity.
- An unresolved dependency is never silently treated as external.
- Node and browser resolution, and import and require conditions, cannot share cached results.
- Exact-file writes do not clean or replace sibling artifacts.
- Exact JavaScript and source-map files use unique same-directory temporary files plus atomic replace.
- CommonJS output assigns only the entry exports to `module.exports` and rejects top-level await in
  the synchronous entry graph while allowing it behind dynamic imports.
- CLI and Node behavior converge through `wake_app`.
- Bundle defaults and semantic option validation are owned only by `wake_app`.

## Evidence

- `crates/wake_resolver/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_app/src/lib.rs`
- `editors/vscode-css/scripts/build.mjs`
- Resolver, bundler, application and Extension Host tests

## Consequences

Wake can build Node-hosted tools without emitting Web application artifacts. The public bundle API
adds platform, format, target, external, minify and outfile options. Node ESM, multiple entries,
plugins and arbitrary-path externals remain outside this first contract.

## Validation

- Execute resolver condition and cache-isolation tests.
- Execute generated CommonJS output with external Node dependencies.
- Run npm API/type tests, architecture checks, vscode-css checks and Extension Host tests.
- Package and inspect all supported VSIX targets.

## Supersedes

None.

## Removal plan

Remove esbuild, its lockfile graph and its vscode-css build calls in the accepting change. No dual
bundle path or compatibility flag remains.
