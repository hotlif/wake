# ADR 0011: Product-owned automatic package releases

- Status: accepted
- Date: 2026-08-17

## Context

Wake has two independent public package families. Seven npm registry packages are built, audited,
and published by `.github/workflows/release-npm.yml` from `v<workspace-version>` tags. The Crab CSS VS Code extension
is a separate multi-platform product: its workflow built five target-specific VSIX files, but only
stored them as temporary workflow artifacts. Its archive and workflow artifact names also repeated
the current extension version.

The repository needs an executable answer to “which package is automatically released?” so adding
a new manifest cannot silently create an unpublished public package.

## Decision

Each public product edge owns exactly one release workflow:

- `.github/workflows/release-npm.yml` owns every non-private package discovered under `npm/*` and
  publishes from `v<workspace-version>` tags;
- `.github/workflows/vscode-css.yml` owns the five target-specific Crab CSS extension packages and
  publishes them as GitHub Release assets from `vscode-css-v<extension-version>` tags;
- npm manifests and `editors/vscode-css/package.json` are the respective version sources of truth;
  release tags must match them exactly;
- `scripts/check-release-coverage.mjs` discovers npm release candidates from manifests and verifies
  that both workflows retain their build, audit, target, credential, and publication contracts.

The VS Code workflow attaches the already-audited VSIX artifacts to a GitHub Release. It does not
rebuild in the release job and does not publish to an extension marketplace.

## Invariants

- Pull requests, branch pushes, and manual verification never publish externally.
- npm and VSIX release tag namespaces are disjoint, so an extension tag cannot start an npm release.
- Platform-specific artifacts are complete and version-aligned before publication.
- npm platform packages publish before `@crab-dev/wake`; all public npm manifests participate in
  the release audit.
- The VS Code extension is `private` for npm and is distributed only through GitHub Releases.
- Release jobs consume previously built artifacts and receive only the permissions they use.
- Re-running an existing release is idempotent: GitHub Release assets are replaced with the
  audited artifact set.

## Evidence

- `npm/*/package.json`: seven current public npm package manifests.
- `.github/workflows/release-npm.yml`: native/JavaScript tarball construction, immutable audit,
  ordered publication, and clean-registry smoke tests.
- `.github/workflows/vscode-css.yml`: five target matrices, VSIX inspection, provenance attestation,
  and GitHub Release attachment.
- `editors/vscode-css/scripts/package-vsix.mjs`: archive version derived from the extension manifest.
- `scripts/check-release-coverage.mjs`: manifest-driven release coverage gate.

## Consequences

Maintainers release npm packages with `vX.Y.Z` and the extension with `vscode-css-vX.Y.Z`. GitHub
Actions uses repository-scoped `contents: write` permission to create the release; no extension
marketplace account, environment, or publication secret is required. Open VSX and the VS Code
Marketplace are not part of this contract.

## Validation

- `npm run release:check`
- `npm run versions:check`
- `npm run vscode:css:check`
- YAML parsing for every workflow
- a local target-specific `npm run package:vsix` smoke
- `npm run architecture:check`
- `git diff --check`

## Supersedes

None.

## Removal plan

Hard-coded VSIX versions in package and artifact names are removed by this change. Marketplace
publishing, its credentials, and its migration bridge are deleted. No temporary bridge or duplicate
publication path remains.
