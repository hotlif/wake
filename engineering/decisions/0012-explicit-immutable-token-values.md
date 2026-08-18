# ADR 0012: Mark cross-module token structures explicitly immutable

- Status: accepted
- Date: 2026-08-17

## Context

Crab component token generators export nested objects so callers can write readable paths such as
`token.primary.color` and `vars['ring.indicator-color']`. Wake can safely evaluate those structures
inside their declaring module, but previously discarded imported objects and arrays because another
importer could mutate the shared JavaScript value before a CSS template read it. The VS Code language
server correctly surfaced the same compiler error after Yarn PnP workspace resolution was repaired.

Flattening every token leaf into a public primitive export would preserve safety but fragment the
generated API. Allowing every imported object would make extracted CSS disagree with runtime
mutation. Editor-only suppression would hide a real build failure.

## Decision

`@crab-dev/css` exposes `defineTokens(value)`. It accepts only recursively pure plain objects,
arrays and the compiler's existing finite primitive value set, returns a deeply readonly type, and
deeply freezes the same structure at runtime without invoking getters or user functions.

`wake_css_in_js` recognizes only the semantic binding imported as `defineTokens` from
`@crab-dev/css`, only when directly initializing a top-level `const`. It evaluates the argument with
the existing allow-listed evaluator and records an unforgeable frozen static-value identity. Frozen
objects and arrays may cross ESM export/import edges; ordinary structures retain the conservative
rejection path. Bundler and language-server consumers propagate the shared static value without
copying marker rules.

Generated Crab component token modules wrap exported structures with `defineTokens`. This is an
atomic contract switch for regenerated source; no source rewrite, plugin diagnostic exemption or
second package entrypoint is introduced.

## Invariants

- Wake never executes a project module, getter, method, constructor or arbitrary function to obtain
  a token value.
- Only import-binding identity from `@crab-dev/css` activates `defineTokens`; spelling and type
  assertions cannot forge frozen provenance.
- Ordinary imported objects and arrays remain unsafe and produce the existing compiler diagnostic.
- Runtime values are deeply frozen and reject accessors, functions, symbols, bigints, exotic
  prototypes and non-finite numbers.
- Object property and array item order remain source deterministic across build and editor paths.
- Wake build and the Crab CSS language server consume the same static-value identity and diagnostic
  implementation.

## Evidence

- `npm/css/`
- `crates/wake_css_in_js/src/lib.rs`
- `crates/wake_css_in_js/src/value.rs`
- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_css_lsp/src/lib.rs`
- `engineering/CRAB_CSS.md`
- `engineering/CSS_LANGUAGE_SERVICE.md`

## Consequences

Generated token APIs keep their nested shape and become safe for cross-module CSS interpolation.
The npm runtime gains a bounded deep-freeze traversal, and the compiler static-value model gains a
frozen provenance variant. Callers that intentionally need mutable runtime objects must continue to
use ordinary exports and cannot interpolate them statically across modules.

This extends ADR 0006's single public Crab CSS contract and preserves ADR 0010's shared
compiler/editor ownership.

## Validation

- Test semantic aliases, shadowing, top-level placement, invalid arguments, ordinary-object
  rejection and frozen-object cross-module propagation in `wake_css_in_js`.
- Build a bundle that imports a frozen nested token and verify extracted CSS; verify runtime deep
  freezing independently in both npm module formats.
- Test ESM/CommonJS runtime parity and TypeScript deep-readonly inference in `npm/css`.
- Test Yarn PnP workspace propagation in `wake_css_lsp` and build the real `rc-button` source.
- Run architecture, Clippy, package, VS Code extension, formatting and diff gates.

## Supersedes

None

## Removal plan

Regenerate component token modules and remove any temporary primitive-export experiments in the
same migration. No compatibility wrapper or editor-only diagnostic path may remain.
