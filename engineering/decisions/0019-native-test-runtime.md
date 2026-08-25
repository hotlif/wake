# ADR 0019: Own a native JavaScript test runtime

- Status: superseded
- Date: 2026-08-21
- Superseded by: [ADR 0020](0020-react-browser-test-runtime.md)

## Context

Wake exposes build, bundle, development-server and documentation behavior through `wake_app`, but
has no JavaScript execution engine or user-facing test runner. The requested testing contract adds
`wake test`, Node APIs and a test-only npm module while requiring Jest 30.4 built-in semantics,
declarative Wake configuration and execution without delegating product behavior to Node or Jest.

The compiler AST is arena-owned and cannot be retained as a runtime or cache value. Test module
mocking and isolation also require a runtime module registry; bundling every suite into one artifact
would erase the module boundaries that those APIs control.

## Decision

Add four explicit owners. `wake_ecma_vm` owns a stable, product-neutral ECMAScript execution facade
and uses the pinned pure-Rust Boa engine as its private bytecode, value and garbage-collection
kernel. `wake_js_runtime` owns Wake parse/codegen preprocessing, module loading and host facilities.
`wake_test` owns test discovery, Jest-compatible APIs, execution policy and structured results.
`wake_test_host` is an internal executable shell used for isolation and native-addon hosting.

Test sources pass through Wake's parser and code generator before execution so TypeScript, JSX,
diagnostics and source identity remain Wake contracts. The resulting JavaScript is compiled to VM
bytecode immediately; arena ASTs and process-local atoms do not enter VM state or persistence.
The Boa API does not cross `wake_ecma_vm`'s public boundary.

CLI and Node callers continue to converge through `wake_app`. The final host transport is a
versioned, authenticated, length-framed local protocol. The first implementation may execute the
same service in-process while the protocol is proven, but that bridge must be removed before this
ADR is accepted and before the stable npm contract is published.

## Invariants

- `wake_ecma_vm` has no file-system, resolver, test-framework, shell or product dependency.
- `wake_js_runtime` owns runtime module identity and never calls the Web bundler.
- Every test suite receives an isolated realm and module registry.
- AST references, atoms, VM values, garbage-collector handles and Node-API handles are never
  serialized or persisted.
- Source locations survive preprocessing and identify the original test or dependency.
- Test discovery, result ordering, seeds, snapshots and coverage artifacts are deterministic across
  supported platforms.
- CLI and Node options, cancellation, results and failures converge through `wake_app`.
- Unsupported host or extension behavior fails with a structured diagnostic; there is no hidden
  Node or Jest fallback.
- Native addons are limited to Node-API versions 1 through 8 and may not import V8, NAN, `node.h` or
  direct libuv ABI.

## Evidence

- `engineering/ARCHITECTURE.md` defines `wake_app` as the shared application boundary.
- `crates/wake_ecma_ast` encapsulates arena-backed `ModuleAst` values.
- `crates/wake_ecma_parser` and `crates/wake_ecma_codegen` already transform JS, TS, JSX and TSX
  while preserving spans.
- `crates/wake_resolver` already owns Node package, workspace and Yarn PnP resolution.
- `crates/wake_node` is built with napi-rs' `napi8` feature.
- Boa 0.21.1 exposes a pure-Rust bytecode VM, garbage collector, realms, modules and Promise job
  queue behind an embeddable context.

## Consequences

The workspace gains a substantial execution and testing product without moving test semantics into
the bundler or shells. Platform artifacts grow to include an internal test host, and release checks
must audit both the Node binding and host. VM conformance becomes a new correctness gate.

The internal engine remains replaceable, but changing it requires replaying the ECMAScript and Jest
conformance matrices. Wake's parser and the VM kernel both parse generated JavaScript; only Wake's
preprocessing result is public, and differential tests must reject syntax or location divergence.

## Validation

- Run architecture checks and crate-focused tests for every ownership change.
- Execute representative JS, TS, JSX, async and module programs rather than asserting emitted text.
- Pin and run the applicable Test262, Jest 30.4 and Node-API v8 conformance manifests.
- Exercise cold and warm test discovery, suite isolation, cancellation and deterministic ordering.
- Run Node API, TypeScript declaration, npm pack and clean-install tests on every published target.
- Run Miri for VM/native-handle lifetimes and Loom for host/TSFN shutdown protocols when those
  implementations land.

## Current acceptance status

At the time this decision was superseded, the ownership boundaries, loopback host protocol,
explicit test module, CLI/Node entry points, isolated suite realms, core assertions and mocks, fake
timers, external snapshots, discovery, projects, sharding and suite workers were implemented. The
stable release gate was not satisfied. The following required evidence or implementation was still
absent:

- pinned Test262 ES2024 and Jest 30.4 differential matrices;
- babel/v8 coverage instrumentation, reporters and thresholds;
- atomic inline snapshot source rewriting against Jest golden fixtures;
- Node-API v1-v8 addon C ABI and its five-platform conformance matrix;
- the complete Node/jsdom host manifest, full Jest CLI option matrix, open-handle reporting, cache
  invalidation and true intra-suite concurrent scheduling;
- the specified Miri, Loom, fuzz, OOM, corrupted-IPC and cross-platform release gates.

Coverage requests and missing inline snapshot rewrites fail explicitly instead of returning empty
or misleading success data. Native `.node` imports return `WAKE_TEST_UNSUPPORTED` until the ABI gate
is implemented.

## Supersedes

None.

## Removal plan

Remove the temporary in-process host bridge before accepting this ADR. When the stable runner is
enabled, migrate the existing JavaScript tests from `node:test` and remove that harness in the same
repository-wide switch. No Node/Jest execution fallback or second test result schema may remain.

`npm/wake/test/api.test.mjs` remains a development-only `node:test` conformance gate while it
exercises the Node-API addon, Worker teardown and real socket lifecycle. It is not a product
fallback. Remove this exception only after the test host implements and passes the pinned Node-API
v1-v8 ABI matrix, then migrate the file to `@crab-dev/wake/test`.
