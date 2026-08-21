# ADR 0018: Aggregate Docs workspaces through isolated application mounts

- Status: accepted
- Date: 2026-08-21

## Context

Wake Docs generated one site or one component workbench per invocation. Component repositories
therefore needed an external process and routing layer to present several package workbenches below
one documentation origin. Folding every package into the site bundle would violate the runtime
isolation established by ADR 0002, create cross-package demo identifiers, and make development
startup proportional to the full repository.

Production output also used a clean-and-rewrite application emitter. Publishing several
independently bundled applications through that path could expose a partially updated tree or fail
on Windows when a generated file remained open.

## Decision

`wake_config` owns only declarative `[[docs.workspace]]` discovery rules. `wake_app` resolves direct
child directories, loads each child configuration, validates names and mount paths, and orchestrates
production or development applications. `wake_docs` remains a single-project generator.

The parent site and every component workbench receive separate generated projects, resolver
configuration, `BuildSession`, bundle state, public directory, public path, and HTML shell. The
shared `wake_dev_server` owns one HTTP port, one watcher orchestration thread, one longest-prefix
mount registry, and one HMR endpoint. Eager mounts build at startup. Lazy mounts build through a
single-flight transition on their first HTML, chunk, or asset request. HMR messages carry the mount
identity so a page responds only to its own application.

Production builds every mount into an isolated staging subtree. After every build and collision
check succeeds, a transactional per-file committer skips equal files, installs changed files,
removes stale files, and restores backups if a later operation fails. The root manifest references
deterministically sorted child manifests. The public build result keeps site `routes` and `demos`
separate and reports workspaces through a new `workspaces` array.

## Invariants

- The site `base_path`, including CLI or Node overrides, prefixes every workspace mount.
- Workspace rules discover direct children only; names are case-sensitive URL-safe path segments.
- `*` and `?` match names but never path separators.
- Workspace mounts are unique and non-overlapping, cannot replace a parent route or output file,
  and are matched before the site by longest base path.
- `/components/<name>/` remains a site route unless the exact configured workbench mount matches.
- Requests reject decoded `.`/`..`, backslashes, encoded traversal, and public symlink escapes.
- Missing file-like requests return 404; an initially failed lazy mount returns 503 without taking
  other mounts offline.
- Changing workspace topology requires a development-server restart; ordinary child configuration
  changes regenerate only that loaded mount.

## Presentation

Aggregated component workbenches default to `embedded`. Embedded presentation retains demo hash,
Args, theme propagation, iframe isolation, and runtime diagnostics while rendering only the preview
surface. `standalone` keeps the complete catalog, toolbar, controls, dialogs, and drawers. Direct
`--mode components` continues to default to standalone.

## Evidence

- `crates/wake_app/src/lib.rs`
- `crates/wake_dev_server/src/lib.rs`
- `crates/wake_docs/src/lib.rs`
- `fixtures/react-docs-workspaces/`
- `fixtures/react-components-yarn-pnp/`
- The focused Rust, Node API, production fixture, lazy-mount, and HTTP routing checks recorded
  in the implementation task.

## Consequences

The development server becomes a generic multi-mount owner, while ordinary application and
single-Docs servers are one-mount uses of the same engine. No new crate dependency edge is added.
Preparing a configured lazy workspace generates its small virtual source tree, but no resolver,
module graph, or bundle session is created until the mount is requested.

The Node `DocsBuildResult` gains a required `workspaces` array. Development events gain optional
`workspace` and `basePath` fields plus `workspaceState`. Existing single-project consumers receive
an empty array and events without workspace fields.

## Validation

Use the real one-site/two-workspace fixture, the Yarn 4.16 PnP aggregate gate, a 51-lazy-mount
startup test, focused Rust tests, Node types and tests, documentation checks, architecture checks,
workspace Clippy/tests, production Docs build, and `git diff --check`.

## Supersedes

None.

## Removal plan

The former single-bundle HTTP routing assumptions are removed in the same change. There is no
parallel aggregate router or compatibility server to remove later.
