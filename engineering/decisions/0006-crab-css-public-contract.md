# ADR 0006: Make Crab CSS the only public CSS-in-JS contract

- Status: accepted
- Date: 2026-08-14

## Context

Wake previously consumed a third-party CSS runtime through application manifests and published UI
components. The repository now owns a typed build-time CSS API, compiler contract, extracted style
artifacts and release package, but retaining source rewriting for the predecessor package would leave
two effective public entrypoints. Some already-published `@crab-dev/rc-*` versions use the new runtime
without fully declaring it, which is visible under Yarn PnP even though hoisted installs hide it.

## Decision

`@crab-dev/css` is the only public CSS-in-JS package recognized by Wake. Repository source, demos,
fixtures, manifests, declarations, documentation and release smoke tests use that package directly.
The compiler does not recognize aliases.

For immutable published component archives only, the loader rewrites the predecessor runtime
specifier to `@crab-dev/css` when the source is a verified public ESM or CommonJS entry of an
`@crab-dev/rc-*` package. Application source, other third-party packages and component internals are
never rewritten.

Components mode may supply `@crab-dev/css` and `lucide-react` from the Wake package only when an
`@crab-dev/rc-*` issuer fails normal Yarn PnP resolution with an undeclared-dependency or unfulfilled-
peer error. The issuer's declared dependency, Yarn top-level fallback and user aliases retain
precedence. This bridge repairs incomplete metadata; it does not add a second CSS API.

## Invariants

- The compiler recognizes only imports bound to `@crab-dev/css`.
- User-facing examples and package manifests contain no predecessor CSS dependency.
- Loader rewriting is restricted to verified public `@crab-dev/rc-*` entrypoints.
- Dynamic values cross the documented CSS custom-property boundary.
- Extracted CSS remains owned by output chunks and loads before the owning lazy JavaScript.
- PnP fallback is issuer-, dependency- and error-scoped and cannot affect application source.
- Package, lockfile, version, pack and release gates include `@crab-dev/css`.

## Evidence

- `npm/css/`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_resolver/src/lib.rs`
- `crates/wake_app/src/lib.rs`
- `scripts/check-versions.mjs`
- `scripts/check-components-pnp.mjs`
- `docs/styles/`

## Consequences

Consumers receive one typed runtime and one compile-time contract. Application projects using
predecessor imports fail through normal resolution or remain ordinary JavaScript rather than being
silently migrated. The bounded loader and PnP bridges add temporary component integration logic, but
preserve the dependency version selected by a correctly declared component.

## Validation

- Run CSS package runtime and type tests plus `wake_css_in_js` tests.
- Run bundler CSS extraction and dynamic chunk execution tests.
- Run `npm run versions:check`, `npm run npm:pack:check` and `npm run pnp:components:check`.
- Run `npm run docs:check`, `npm run docs:build` and inspect the demo output.
- Scan tracked source and manifests for predecessor package specifiers.

## Supersedes

None.

## Removal plan

Republish every supported `@crab-dev/rc-*` package with direct `@crab-dev/css` imports and complete
`@crab-dev/css` and `lucide-react` metadata. Once normal and isolated PnP fixtures succeed without
migration, remove `migrate_crab_component_css_runtime`, `PnpDependencyFallback`,
`components_pnp_dependency_fallbacks`, their tests and this temporary part of the decision. The single
public CSS package decision remains in force.
