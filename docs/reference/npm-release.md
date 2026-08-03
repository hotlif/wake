# npm release runbook

Wake publishes six immutable packages at one version: the JavaScript package and five
platform packages. The repository root is private and is never published.

## First release

1. Confirm the operator has publish access to the `@crab-dev` scope.
2. Create a one-time granular npm token limited to the six Wake package names and the
   shortest practical expiry. Store it as the `npm-release` environment secret
   `NPM_TOKEN`.
3. Push `v0.1.0` only after `npm run versions:check`, the Rust gates, Node gates, and
   local pack checks pass. The release workflow builds on the target systems, checks
   the glibc/macOS baselines, verifies and attests all six tarballs, publishes platform
   packages first, and publishes the JavaScript package last.
4. Do not retry a partially incorrect `0.1.0` by overwriting it. Fix the issue and
   release a patch version with all six manifests and Cargo updated together.
5. After all registry smoke jobs pass, configure GitHub trusted publishing for each of
   the six npm packages, remove the `NPM_TOKEN` secret, and revoke the one-time token.
   Later releases use OIDC and npm provenance.

## Retirement gate

Do not deprecate the legacy package from the release job. Run the separate
`Retire Crustify package` workflow only after every Node/platform registry smoke job is
green. It verifies the replacement version again and applies the fixed deprecation
message without unpublishing historical versions.

The legacy source repository is handled in its own pull request: scan reverse
dependencies, create an archive tag, remove active workspace and release references,
and preserve Git history. Wake's repository does not delete or rewrite that external
repository.