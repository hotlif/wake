# ADR 0025: Wake-native federation contract and identity boundary

- Status: accepted
- Date: 2026-09-01
- Amended by: [ADR 0032](0032-federation-development-snapshot-leases.md)

## Context

Wake needs browser-side remote modules, page-level dependency reuse, React major-version isolation,
declaration discovery, and source-mapped development without adopting the Webpack/Rspack container
ABI. The current chunk graph has only one `Entry` root plus dynamic-import roots, while the emitted
runtime namespace token is derived from one entry path and module count. Those process/build-local
identities cannot safely identify a remote container, expose, declaration bundle, or share provider
across independently produced builds.

Putting manifest values directly in `wake_bundler`, config, Node bindings, or browser JavaScript
would create several competing wire formats. Putting URL parsing, HTTP, hashing, resolver package
objects, or runtime handles in a shared model would instead invert the existing compiler/product
dependency direction.

## Decision

1. `wake_federation_contract` is the sole Rust owner of Wake Federation v1 DTOs: normalized config,
   immutable manifests, production lock references, stable container/module/package identity,
   sharing policy, asset and type metadata, stable error codes, and versioned development updates.
2. The crate is I/O-free and may depend only on serialization libraries. It does not depend on any
   Wake crate and owns no AST, resolver, filesystem, URL fetch, hash implementation, browser, Node,
   runtime execution, cache, or product lifecycle.
3. The manifest schema is `wake.federation.manifest.v1`; the browser container ABI is
   `wake.federation.v1`. Consumers reject an unknown schema or ABI before executing remote code.
4. Cross-build modules use `(container, buildId, expose, generation)`. Container-local numeric module
   IDs remain private and never become persistent, global, or cross-container keys.
5. Manifests use ordered maps and expose deterministic validation. Canonical build-identity material
   excludes deployment URLs, development metadata, and `buildId` itself; the bundler owns the hash
   algorithm and final identifier spelling.
6. Assets carry content hash, exact SHA-384 SRI, MIME and size. Declaration artifacts carry their
   JavaScript `buildId`; a mismatch is a hard `FED_TYPE_BUILD_MISMATCH` failure.
7. Shared dependencies are opt-in and carry resolver-stable package/version/context/variant identity.
   Policy records scope, strictness, singleton, fallback, coherence group and optional owner; runtime
   selection and semver evaluation remain outside the DTO crate.
8. React boundary metadata is explicit: host-rendered exposes use no ShadowRoot and their scope must
   atomically require `react`, both JSX runtime subpaths, `react-dom`, and `react-dom/client` as one
   singleton coherence group with identical owner semantics. Isolated exposes use an open ShadowRoot
   and a non-default scope. Contextual config defaults are normalized before the bundler receives the
   contract.
9. `wake_bundler::BuildOutput` records the sorted, deduplicated container-local module owners of each
   emitted binary asset. Duplicate emissions of the same content-addressed file union their owners.
   Manifest producers project those local IDs onto initial/dynamic chunk closures; only stable chunk
   file identities enter `buildId` material, and numeric module IDs are never serialized on the wire.
10. Production federation builds always collect source maps without changing JavaScript emission.
    The default build mode writes rewritten maps only to
    `.wake/federation/source-maps/<container>/<buildId>`; explicit public source maps place them in
    the deployment output and Manifest as before. Hidden maps are debug artifacts, not Manifest,
    lock, public file-list, or canonical build-identity inputs.
11. The bundler labels the container-local module owners of ordinary unscoped CSS separately from
    CSS Modules and Wake CSS-in-JS. Wake producers reject a `host-rendered` expose when either its
    initial or transitive dynamic closure reaches such an owner, unless that expose explicitly sets
    `allowGlobalCss`. This is a producer build policy and does not add a Manifest wire field.
12. Wake-generated development and production bootstraps own the first page broker creation. Before
    creating it, they read only a CSP nonce from the matching module script or, with higher
    precedence, from the page-owned `Symbol.for('wake.federation.runtime-options.v1')` record. That
    record is fail-closed and may contain exactly one `nonce` data property; it cannot inject a
    transport, alternate global, limits, mode, or any other runtime authority.
13. The page broker owns isolated stylesheet placement by immutable
    `(container, buildId, expose, generation)` identity. An isolated bridge attaches its open
    ShadowRoot before loading remote module code; initial and later chunk styles are ordered and
    replicated to every active target. Missing or ambiguous isolated ownership fails with
    `FED_STYLE_LOAD` and never falls back to `document.head`. Detach is reference-counted per root
    and removes nodes created by the default transport.
14. Every production remote lock records an explicit `hasExposes` value derived from the validated
    Manifest. Exposed remotes require a declaration artifact bound to the same `buildId`; shared-only
    remotes may omit declarations. Lock generation rejects every singleton offer or requirement
    without an explicit owner, and the browser revalidates expose presence, declarations, and
    singleton ownership against the integrity-pinned Manifest before registration.
15. The development broker separates its monotonic control-plane build/generation cursor from the
    active code Manifest. A `types-only` update advances only that cursor. Code-bearing updates move
    subsequent remote loads to a newly fetched Manifest, while every previously accepted immutable
    Manifest remains cached by `(remote, buildId)` for the page lifetime so a live old requester can
    integrity-load its own lazy asset closure. Canonically identical updates for the current
    generation are idempotent at both acceptance and action dispatch; a same-generation conflict
    fails closed. The Manifest-observed cursor and applied control-frame cursor are separate: the
    first same-generation frame may catch up only across a continuous `oldBuildId -> newBuildId`
    edge. A superseded Manifest flight cannot commit a validation error into the current revision.
16. A generated `SharedFallback` dynamic root owns an explicit bundler resolution context. Every
    module in that root's static closure resolves allowlisted shared requests locally, allowing an
    interdependent coherence group to initialize atomically before a broker share context exists;
    the same requests from Application or Expose contexts remain broker-owned. Root configuration
    and per-module context enter retained graph/link/cache identity. If one physical module is
    reachable through both contexts, the build fails closed instead of choosing edge semantics by
   traversal order.

## Invariants

- One schema constant and one DTO owner serve Rust producers and JavaScript consumers.
- Serialization is camelCase on the wire; config accepts the documented snake_case aliases.
- Unknown manifest/config fields fail closed and validation errors have deterministic order.
- Manifest identity contains no arena pointer, interner identity, process-local module ID, path handle,
  network response, executable factory, or runtime object.
- An expose authorizes only binary assets whose owner modules occur in its initial or transitive
  dynamic chunk closure; CSS remains ordered by the chunks' `styles` relation.
- Hidden and public source-map modes produce byte-identical JavaScript, content hashes and `buildId`;
  hidden maps have no `sourceMappingURL` discovery edge and never enter the public output closure.
- Host-rendered global-CSS validation uses the full expose chunk/module closure and fails closed in
  development and production; generic and isolated boundaries retain their existing CSS behavior.
- Absence of a bootstrap CSP nonce preserves existing loading behavior; an explicitly installed
  runtime-options symbol with an invalid object, nonce, accessor, or extra key fails before the page
  broker is created.
- Generic and host-rendered styles retain page-head placement. Isolated initial and lazy styles have
  one Manifest identity and may only enter attached open ShadowRoots; a later attachment hydrates
  the ordered style history for that identity.
- Isolated props and event details are copied only from own enumerable data descriptors without
  invoking accessors. Arrays are dense structured values, and slots are actual DOM Nodes whenever
  the browser exposes a `Node` constructor; React values, refs, functions and Node-like spoofs fail
  before the remote lifecycle runs.
- Production consumers never infer expose presence from optional declarations or assets. A missing
  `hasExposes`, a lock/Manifest presence mismatch, an exposed remote without build-bound types, or an
  ownerless singleton fails closed; the Manifest SHA-384 binds the reviewed exposes and owners.
- A deployment origin or development generation cannot change otherwise identical build material.
- Production lock entries require HTTPS and exact per-resource SHA-384 integrity.
- A host-rendered config or manifest with a missing or split React five-member group fails closed;
  generic and isolated exposes do not inherit that completeness requirement.
- A historical requester can load only assets authorized by its previously accepted exact-build
  Manifest. It cannot make the broker fetch arbitrary retired build metadata, while new remote loads
  follow the active Manifest selected by the latest code-bearing development update.
- A `types-only` update never retires an active container, changes the execution revision, or clears
  evaluated modules. Duplicate canonical Federation browser-update frames neither remount nor reload twice;
  stale Manifest fetch, schema, or integrity failures cannot poison a newer generation.
- `SharedFallback` local resolution never leaks into Application or Expose consumers, and one
  physical module cannot acquire both local-fallback and broker-owned shared-edge semantics.
- No federation import causes network or filesystem I/O from the contract crate.

## Evidence

- `crates/wake_bundler/src/chunk.rs`: `BucketKey` currently models `Entry`, `Shared`, and `Async` roots.
- `crates/wake_bundler/src/incremental.rs`: `build_token` hashes entry path plus module count for one
  emitted runtime and therefore is not a cross-build identity.
- `crates/wake_federation_contract/src/manifest.rs`: versioned DTO validation and canonical identity
  material are pure serialization transforms over owned data.
- `crates/wake_federation_contract/src/config.rs`: normalized config enforces remote URL, expose,
  React scope/ShadowRoot, shared package and coherence constraints without resolving or reading files.
- `crates/wake_app/src/federation_lock.rs`: production lock generation validates Manifest-only
  policy, records expose presence, and permits shared-only remotes without declaration artifacts.
- `npm/wake/federation.mjs`: production registration compares the required lock metadata with the
  integrity-verified Manifest before accepting its exposes or shared providers; development state
  retains exact-build Manifest authorization separately from the control-plane cursor.
- `npm/wake/test/federation.test.mjs`: browser runtime tests cover types-only in-flight preservation,
  isolated/full update routing, old-build lazy JavaScript and CSS, stale-flight isolation,
  HTTP-before-WebSocket control catch-up, and duplicate update idempotency.
- `crates/wake_bundler/src/tests.rs` and `crates/wake_app/src/federation.rs`: interdependent shared
  members stay inside the explicit `SharedFallback` closure, ordinary consumers retain broker
  bridges, and a module reached through both resolution contexts is rejected deterministically.
- `crates/wake_federation_contract` tests cover stable JSON fields/error codes, identity independence,
  type/build drift, SRI/MIME failures, production HTTPS locks and development-update normalization.

## Consequences

Bundler, config, application, dev-server and Node/browser adapters can share one format without
depending on one another. The extra crate and version checks add a small coordination cost, and
changing a wire field now requires an intentional schema/ABI decision. Serialization alone does not
provide semver solving, hashing, loading, sandboxing, or trust; those capabilities remain with their
domain owners. Wake Federation v1 is Wake-native and does not claim Webpack/Rspack ABI compatibility.

## Validation

- Affected Rust crates pass `cargo +1.95.0 test --locked --offline` and all-target Clippy with
  warnings denied; the workspace passes `cargo +1.95.0 fmt --all -- --check`.
- Federation and React browser-runtime suites pass 54 Node tests; Wake's TypeScript declarations
  pass `npm:typecheck:wake`.
- The ignored real-browser suite passes three Chromium scenarios: development remote/async maps,
  a production minified lazy chunk with a public map, and real React 18 host rendering alongside
  isolated React 17/18 ShadowRoots.
- `architecture:check`, `architecture:test`, `docs:check`, immutable Yarn install/lock validation,
  and `git diff --check` pass.

## Supersedes

None.

## Removal plan

No compatibility bridge is introduced. Existing bundler namespace tokens and numeric module IDs stay
container-local; federation consumers must not promote them to global identity. Config, bundler, Node
and browser adapters adopt this schema atomically and must not retain a second public DTO or wire
format.
