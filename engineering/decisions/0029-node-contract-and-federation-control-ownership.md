# ADR 0029: Node contracts and Federation control stay product-owned

- Status: accepted
- Date: 2026-09-02

## Context

Wake exposes one product through the Rust CLI, the Node addon, CommonJS/ESM wrappers, the npm CLI,
TypeScript declarations, and release tarballs. Several boundaries had drifted independently:

- Rust could return output `kind` strings that the TypeScript union did not admit.
- The Rust Federation error contract contained codes absent from the browser runtime and Node error
  type.
- `FederationUpdated` reached the addon but the CommonJS event adapter silently discarded it while
  documentation promised the event.
- `federation init` was implemented inside the Rust CLI, and the npm CLI exposed neither init nor
  lock generation.
- source-tree type checks did not prove that ESM declaration files or Federation subpaths survived
  packing and NodeNext resolution.

These are boundary ownership failures rather than isolated presentation bugs.

## Decision

1. `wake_app` owns Federation initialization and production lock services. It owns project discovery,
   no-clobber declaration publication, error codes, remote validation, and atomic lock publication.
   Rust CLI and Node/NPM frontends parse arguments, invoke that service, and present its result.
2. `wake_app::OutputFileKind` is the exhaustive Rust owner of public output inventory kinds. Every
   Rust producer constructs a variant, serialization comes from its stable `as_str()` mapping, and
   the Node addon uses an exhaustive match with no wildcard. The npm declaration is a closed union
   whose value set must exactly equal the Rust mapping.
3. `wake_federation_contract::ErrorCode` and its `as_str()` mapping own the complete `FED_*` set. The
   browser runtime object and its ESM declaration must contain exactly that set. `WakeErrorCode`
   includes the Federation contract plus the three initializer codes
   `WAKE_FED_INIT_CONFIG`, `WAKE_FED_INIT_IO`, and `WAKE_FED_INIT_CONFLICT`.
4. Every native development-server event requires an explicit JavaScript adapter branch, public type,
   and behavior test. Unknown active-server events surface a `WAKE_INTERNAL` diagnostic instead of
   disappearing. `federationUpdated` is forwarded without reshaping its identity fields.
5. The ESM-only `@crab-dev/wake/federation` subpath owns one `federation.d.mts` declaration paired with
   `federation.mjs`; no copied `.d.ts` shadow is retained. Package exports are the source of public
   targets.
6. Release verification runs Federation runtime tests and Wake TypeScript checks. Pack checks derive
   required files from `main`, `module`, `types`, `exports`, and `bin`; actual tarball audit repeats
   the critical target check. A consumer outside the PnP source tree installs local tarballs and runs
   NodeNext TypeScript with `skipLibCheck: false` plus CJS, ESM, CLI, build, and Wake Test smoke.
7. Native-backed `BuildContext`, `DevServer`, and `TestContext` instances are factory-owned. Their
   public declarations have private constructors, and the CommonJS implementation rejects direct
   JavaScript construction before an invalid handle can escape. `WakeError` declares its actual
   product constructor, and optional Node operations remain optional in both JavaScript and types.
8. `FederationManifestWire` describes the canonical Rust JSON artifact, including required `null`
   values for absent `Option` fields. `FederationManifest` describes the validated runtime value,
   where those values are normalized to `undefined`. Transport results stay `unknown` until that
   validation boundary. These two lifecycle shapes must not be conflated.
9. Event compatibility covers payload fields as well as discriminants. Rust serialization, the
   JavaScript projection, and the public event interface must change in the same slice.
10. TypeScript's unbounded `number` cannot encode Rust integer widths. Public declarations document
    the accepted ranges; the Node deserialization boundary remains authoritative and rejects
    fractional, negative, or out-of-range values before application services run.

## Invariants

- A frontend cannot own a second Federation initializer or lock implementation.
- Adding an output kind breaks exhaustive Rust/Node matches and the cross-language set gate until the
  TypeScript contract is updated.
- Adding or removing a Federation error code fails the Rust/runtime/declaration equality gate.
- A native event is never silently ignored by the JavaScript development-server adapter.
- A public native-handle class cannot be constructed outside its named factory.
- Canonical Manifest JSON may contain `null`; a normalized runtime Manifest may not.
- Event discriminants and their complete public payload field sets stay equal across Rust,
  JavaScript, and TypeScript.
- Numeric Node options are range-checked at deserialization even when TypeScript can only spell
  their carrier type as `number`.
- Every target reachable through a published package entry is present and type-resolvable in the
  packed artifact.

## Evidence

- `crates/wake_app/src/federation_init.rs` owns declaration initialization;
  `crates/wake_app/src/federation_lock.rs` owns lock generation.
- `crates/wake_app/src/output.rs` owns `OutputFileKind`; `crates/wake_node/src/lib.rs` contains the
  exhaustive Node wire mapping.
- `npm/wake/index.cjs`, `index.mjs`, `index.d.ts`, and `bin/wake.mjs` expose the same control services
  and development events; context construction is guarded in both JavaScript and TypeScript.
- `npm/wake/federation.mjs` and `federation.d.mts` mirror the Rust Federation errors and distinguish
  canonical wire Manifests from normalized runtime Manifests.
- `scripts/check-architecture.test.mjs`, `check-npm-packs.mjs`,
  `check-npm-consumer.mjs`, and `check-release-coverage.mjs` enforce the export, event-payload,
  constructor, and Manifest lifecycle boundaries.

## Consequences

Public enum and error additions now require coordinated frontend changes in the same slice. This is
intentional compatibility work, not optional duplication. External consumer verification costs more
than a source-tree type check, but it catches export-map, declaration-suffix, packing, PnP leakage, and
installed-resolution failures before publish.

The Node cancellation signal can reject an in-flight control task, but the current synchronous Rust
lock fetcher has no cooperative per-request cancellation point. That limitation remains explicit; it
does not permit a second JavaScript lock implementation.

## Validation

- `cargo test -p wake_app --lib`
- `cargo test -p wake_node --lib`
- `cargo test -p wake_cli --test cli_output federation_init`
- `corepack yarn npm:test:wake`
- `corepack yarn npm:typecheck:wake`
- `corepack yarn npm:pack:check`
- `corepack yarn npm:consumer:check`
- `corepack yarn release:check`
- `corepack yarn architecture:test`
- `corepack yarn architecture:check`

## Supersedes

None.

## Removal plan

Delete the former CLI-local initializer in the same change that introduces the application service.
Do not retain compatibility copies of Federation declarations or free-form output-kind strings. A
future generated contract may replace the equality gate only if Rust, runtime, and TypeScript are all
generated from that single checked-in owner.
