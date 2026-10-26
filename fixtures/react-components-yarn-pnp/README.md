# Components PnP smoke contract

`scripts/check-components-pnp.mjs` packs the current Wake packages and installs them into a fresh
Yarn PnP project. The temporary lockfile may select any Lucide version admitted by Wake's dependency
range, independently of the repository lockfile.

The published Wake manifest and root workspace must keep `@crab-dev/rc-tree` at the reviewed `0.1.2`
release until a newer dependency graph passes this complete fresh-install gate. Tree `0.1.4` pulls in
`rc-config-provider@0.0.2`, whose published manifest omits React, and `rc-token-semantic@0.0.4`, whose
theme reads `material`, `purple`, and opacity keys absent from its declared `rc-token-global@0.0.2`.
PnP rejection and the static `defineTokens` error are correct for that graph. Do not hide either error
with a resolver fallback, a fixture-only resolution, or a weaker CSS evaluator. Restore a wider Tree
range only after the upstream metadata and token graph are compatible and this gate passes without
fixture-only substitutions.

The runtime smoke must recognize both Lucide's positional icon-name factory and its object metadata
factory. It must still require exactly one matching icon module, a non-null default component export,
and a Lucide barrel exposing every icon used by the workbench. An absent icon or broken export must
fail; a change between these supported factory forms must not produce a false missing-icon diagnostic.

Run the smoke regression with `wake test scripts/components-runtime-smoke.test.mjs --serial`, then
run `corepack yarn pnp:components:check` with the current native build. CI runs the regression in its
architecture job and the complete fresh-install fixture in the Node 24 job.
