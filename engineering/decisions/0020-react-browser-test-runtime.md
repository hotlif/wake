# ADR 0020: Own a React-first browser test runtime

- Status: proposed
- Product maturity: experimental
- Date: 2026-08-24

## Context

ADR 0019 selected a pure-Rust Boa VM and a Jest 30.4 compatibility target. Implementation evidence
showed that this combined four independently large products: an ECMAScript engine, a Node/jsdom
host, the Jest compatibility surface, and a test orchestrator. Coverage, inline snapshots,
Node-API, DOM behavior and the upstream compatibility matrices remained incomplete, while the
compatibility promise prevented Wake from choosing APIs and execution semantics specifically for
React applications.

Wake instead needs a test product whose source shape is familiar to Jest users but whose contract
is owned by Wake. React component correctness also needs two different kinds of evidence: a fast,
isolated DOM realm for most feedback and a real browser for layout, native input, hydration,
screenshots and browser-only APIs. Neither Deno's test runner nor an external Jest or Node process
owns either product requirement.

The workspace already owns TypeScript/JSX preprocessing, resolution, source maps, incremental
graphs, CLI/Node application convergence and an isolated test-host process. It can reuse these
boundaries while replacing the abandoned compatibility target and engine.

## Decision

Wake Test is a Wake-native, React-first framework. `@crab-dev/wake/test` exposes explicitly
imported, Jest-shaped primitives such as `describe`, `test`, hooks and `expect`; a React entry owns
rendering, async `act`, cleanup and user-oriented DOM helpers. API familiarity does not imply Jest
configuration, runner, reporter, plugin, snapshot, mock, CLI or JSON compatibility. Wake does not
export a `jest` namespace and does not execute the Deno, Node or Jest test runners.

The execution system has seven explicit owners:

- `wake_ecma_vm` is the only Wake crate allowed to depend directly on the checksum-locked
  `deno_core` package from crates.io; `deno_v8` remains a transitive crates.io dependency. It owns
  a product-neutral embedded V8 isolate/realm, Promise job queue, termination and stable diagnostics
  facade. Deno CLI and `deno test` are not product dependencies.
- `wake_js_runtime` owns Wake module identity, preprocessing, resolution adapters, host operations
  and lifecycle of the fast DOM environment. Its DOM implementation is a pinned, private,
  Wake-adapted substrate; it is not a public jsdom, Happy DOM or browser compatibility promise.
- `wake_test_browser` owns system Chromium-family discovery, version reporting, launch, CDP
  transport, isolated BrowserContexts, authenticated loopback resources, real input, network
  interception, screenshots and precise V8 coverage. It owns no discovery, assertion, snapshot or
  reporter policy and never depends on `wake_test`.
- `wake_test_contract` is the only owner of serializable test options, results, diagnostics and the
  versioned test-host wire. It depends on no other Wake crate and contains no discovery,
  scheduling, VM, DOM, browser or process-lifecycle behavior.
- `wake_test` is the sole owner of discovery, scheduling, the authoring API, React integration,
  function and network mocks, the modern async clock, snapshot formats, coverage normalization,
  watch invalidation and result construction. It consumes `wake_test_contract` and may compose
  `wake_js_runtime` and `wake_test_browser`.
- `wake_test_host` is the sole persistent isolation and session-IPC owner. It authenticates and
  versions requests, contains VM or browser crashes, propagates cancellation and closes resources,
  consumes the wire from `wake_test_contract`, and delegates all test semantics to `wake_test`.
- `wake_app` remains the shared product boundary through which CLI and Node callers configure,
  start, observe, cancel and close test sessions. It links only `wake_test_contract`, launches the
  test host as a separate process and never links the runner, JavaScript runtime or embedded V8.

The default fast environment creates a fresh V8 realm, module registry, Window and Document per
suite. The DOM substrate is installed before React and React DOM evaluate, preserves same-realm
constructor identity, sets `IS_REACT_ACT_ENVIRONMENT`, delegates timers and network to Wake, and is
fully torn down after the suite. Its compatibility claim is limited to Wake's versioned React/DOM
conformance manifest.

The browser environment starts or attaches to an explicitly selected Chrome, Edge or Chromium
executable. One browser process may be reused, but every suite receives a fresh BrowserContext and
page. The executable identity and version are part of diagnostics, results and cache identity.
Browser binaries are not placed in Wake's existing platform packages; CI supplies and pins the
browser used for release evidence. `engineering/system-browser-conformance.json` pins one shared,
exact Chromium-family major across the five release targets and accepts patch/build variation
within that major.
The pin is admission policy for CI and release conformance only: ordinary local test runs continue
to accept any compatible system Chrome, Edge or Chromium and always report its full CDP identity.

Tests within one DOM are sequential. Suites may run in parallel in isolated realms or
BrowserContexts. Runtime module automocking, Jest-style hoisting, legacy fake timers and inline
snapshot rewriting are not part of the contract. Module replacement is explicit and resolved
before module evaluation; browser network mocking occurs at the driver boundary. Coverage uses V8
ranges mapped through Wake source maps into a Wake-owned schema rather than Babel/Istanbul
instrumentation semantics.

Related selection and watch invalidation consume the exact owned module graph compiled by
`wake_js_runtime`; they do not reuse bundler chunk graphs or `wake_graph` liveness records. The
runtime sidecar separates logical module IDs from physical watch paths and contains only sorted,
owned records. `wake_test` owns suite-to-module and module-to-suite indexes. Dynamic loads, current
PnP resolution, resolution misses and structural inputs mark the graph opaque; Wake then selects
all candidate suites with a diagnostic rather than risk a false-negative. Full PnP precision is a
separate resolver slice and remains a release gate.

`--changed` has one deterministic Wake meaning: tracked staged and unstaged paths are compared to
`HEAD`, non-ignored untracked files are added, rename detection is disabled so delete and add are
both visible, and an unborn `HEAD` uses the index plus untracked files. Missing Git or a root outside
a work tree is `WAKE_TEST_DISCOVERY`; Wake does not guess an upstream branch or emulate Jest SCM
heuristics.

The host protocol is atomically version 3 and its complete serializable schema and frame codec live
in `wake_test_contract`. `StartWatch` carries the context's frozen options and creates the sole
recursive native watcher; `WatchControl` owns `all`, `failed`, `path`, `name`, `updateSnapshots` and
`rerun` transitions. The host coalesces changes, interrupts an obsolete run, and emits ordered
unsolicited run events on the authenticated session. JavaScript owns no file watcher and
synthesizes no test-result events. Every rerun emits a compiled graph into a fresh realm or
BrowserContext while retaining only discovery and compiled dependency artifacts.

## Invariants

- Wake Test has one semantic owner in `wake_test` and one serializable contract owner in
  `wake_test_contract` across CLI, Node and the host.
- `wake_test_contract`, `wake_app`, `wake_cli` and `wake_node` have no transitive dependency on the
  test runner, JavaScript runtime, VM, browser driver, `deno_core`, `deno_v8`, `serde_v8` or `v8`;
  those engine-bearing dependencies terminate in the separately packaged `wake_test_host`.
- Only `wake_ecma_vm` may depend directly on `deno_core`; engine handles never cross its public
  boundary.
- Third-party Rust and JavaScript sources or binaries are never copied into the repository. Rust
  dependencies resolve from crates.io under `Cargo.lock`; JavaScript dependencies resolve from the
  npm registry under `package-lock.json`. Formal build steps are locked and offline after a separate,
  checksum-verifying dependency or artifact preparation step.
- `wake_test_browser` may be consumed by `wake_test` but cannot depend on it or implement framework
  semantics.
- Each suite owns one isolated realm/module registry or BrowserContext/page. No DOM node, V8
  handle, CDP session identifier, process-local atom or arena reference enters IPC or persistence.
- The fast DOM never resolves project modules, performs unmediated network I/O, captures real timer
  functions outside the Wake clock, or evaluates scripts through a second loader.
- Real Chromium is authoritative for layout, CSS rendering, native input, focus, navigation,
  screenshots and browser-sensitive hydration. Unsupported fast-DOM behavior fails or is routed to
  browser validation; it is not silently approximated as browser evidence.
- Every browser page fixes `prefers-reduced-motion` to `reduce`; the same value participates in the
  screenshot rendering-profile hash and artifact metadata, so host motion settings cannot reuse an
  incompatible baseline.
- React rendering and user interactions settle through async `act`; suite teardown removes roots,
  DOM state, timers, storage, intercepted requests and pending handles.
- Source locations and coverage map back to original JS, TS, JSX or TSX sources.
- Discovery, ordering, seeds, textual snapshots and normalized coverage are deterministic. Cache
  identity includes compiler options, framework/runtime versions, environment, engine or browser
  version, DOM adapter version and every semantic configuration input.
- A changed path can select fewer suites only when the owned reverse index proves the relationship;
  topology changes, deleted/created paths, resolver inputs and opaque edges rediscover or select all.
- `TestContext.run()`, `startWatch()` and `stopWatch()` remain parameterless public context methods;
  interactive selection is an internal protocol command and does not enlarge the npm API.
- Browser resource origins and host sessions are local, authenticated and closed on success,
  failure, timeout, cancellation or child-process crash.
- Unsupported Jest, Node, Deno, DOM or Chromium extension behavior produces a structured Wake
  diagnostic; there is no hidden external runner or second VM backend.

## Evidence

- `engineering/ARCHITECTURE.md` and the executable boundary policy establish `wake_app` as the
  shared CLI/Node application boundary.
- `wake_ecma_vm` uses checksum-locked `deno_core` 0.410.0 from crates.io and exposes owned Wake
  values and diagnostics rather than V8 handles.
- `wake_test_browser` has independent browser discovery, CDP, BrowserContext, resource-origin,
  input, screenshot and precise-coverage seams and has no Wake workspace dependency.
- `wake_resolver`, the parser and code generator already own package/Yarn PnP resolution and
  JS/TS/JSX/TSX preprocessing with source identity.
- React 19 requires async `act` and `IS_REACT_ACT_ENVIRONMENT`; React removed the other legacy
  `react-dom/test-utils` APIs and recommends user-oriented DOM testing.
- Simulated DOM implementations do not provide authoritative layout, navigation or rendering, so
  browser-sensitive behavior requires executed Chromium evidence.

## Consequences

Wake no longer promises to be a Jest 30.4 implementation. Existing experimental tests using the
`jest` namespace, Jest-specific configuration or snapshots require migration to Wake APIs and
formats. This is an intentional breaking replacement before the test product becomes stable.

Embedding V8 improves ECMAScript and React runtime fidelity but replaces the pure-Rust-engine
claim with a Rust facade over a registry-resolved native engine. Source provenance, licenses,
lockfile checksums, toolchain reproducibility, supported libc baselines and binary size become
release gates. Repository-local forks or vendored copies are not a fallback; a required upstream
change must be published through an approved registry source before Wake consumes it.

The fast DOM provides short feedback but has a deliberately smaller truth domain than Chromium.
Maintaining both environments adds differential testing and cache inputs; it does not create two
framework implementations because they share the Wake test kernel and result model.

Browser mode requires a compatible local Chromium-family executable. System-browser variance is
made visible through result metadata. CI and the blocking prepublish matrix reject a browser whose
post-CDP identity does not match the repository's exact-major conformance manifest; a hosted-runner
upgrade therefore requires a reviewed manifest bump and a green five-platform matrix. This does not
restrict ordinary users to that major. The browser is kept out of the current npm platform-package
size budget.

Native-addon hosting, full Node compatibility, third-party Jest runner/reporter/environment ABI,
legacy timers, Babel coverage and Jest golden output are removed from the stable-release burden.

## Validation

- Run `npm run architecture:test` and `npm run architecture:check`; reject direct `deno_core`
  access outside `wake_ecma_vm`, any reverse dependency from `wake_test_browser` to `wake_test`,
  any runner/runtime/V8 package in the all-feature, all-target normal/build dependency closure of
  `wake_test_contract`, `wake_app`, `wake_cli` or `wake_node`, or a host closure missing the
  authoritative contract and runner. Also reject repository-local third-party source/binary trees,
  non-registry dependencies, incomplete lockfile provenance and network-capable formal build hooks.
- Pin engine source/checksums and run the selected Test262 manifest for supported ECMAScript,
  module, Promise, termination and source-location behavior.
- Run React 19 fixtures for createRoot, hooks/effects, portals, controlled forms, Suspense, lazy
  modules, error boundaries, async `act`, SSR parsing and hydration diagnostics.
- Run the fast-DOM manifest for realm identity, MutationObserver, focus, selection, forms, cleanup,
  timers and intercepted network; differentially execute every browser-sensitive fixture in
  Chromium and document the accepted fast-environment boundary.
- Execute real keyboard/pointer/default actions, CSS/layout, hydration, accessibility, screenshots,
  network interception and V8 coverage in isolated BrowserContexts.
- Map runtime errors and coverage through Wake source maps to original TSX and compare cold, warm
  and watch reruns for identical semantics and deterministic ordering.
- In temporary Git repositories, cover staged, unstaged, ignored, untracked, rename/delete, unborn
  `HEAD`, nested roots, missing Git and non-repository behavior. Exercise direct, transitive, shared,
  opaque and structural reverse-dependency selection without false-negatives.
- Exercise protocol-v3 watch start/stop/control, filesystem debouncing, obsolete-run cancellation,
  ordered unsolicited events, repeated runs on one TCP session and fresh realm state. CLI PTY tests
  cover `a`, `f`, `p`, `t`, `u`, `r` and `q`; non-TTY watch persists until signal or close.
- Exercise timeout, infinite loop, unhandled rejection, browser/host crash, cancellation, malformed
  IPC, resource-origin authentication and idempotent shutdown without leaking child processes,
  ports, pages or profiles.
- On Windows x64, macOS x64/arm64 and glibc 2.28 Linux x64/arm64, validate engine startup, system
  browser discovery or explicit paths, the pinned post-CDP browser major, CDP lifecycle and React
  smoke fixtures. A mismatch fails without downloading or selecting a fallback browser.
- Audit registry licenses/SBOM, V8 artifacts, native symbols, GLIBC baseline, npm pack whitelist and
  size limits. Browser executables and third-party source or binary copies must not enter the
  repository or existing platform tarballs.

The public entry remains experimental while any applicable gate above is missing. ADR acceptance
requires the repository to contain no active Jest/Boa/jsdom/Node-API compatibility path or stale
release promise and requires every supported platform gate to pass from clean-install artifacts.

## Supersedes

[ADR 0019](0019-native-test-runtime.md).

## Removal plan

Replace the Boa dependency and old Jest runtime rather than retaining selectable backends. Remove
the `jest` namespace, Jest configuration/CLI/JSON mappings, Jest snapshot header and inline rewrite
contract, Babel coverage, legacy fake timers, the hand-written jsdom shim and Node-API/native-addon
conformance requirements. Rename or replace tests, docs and diagnostics that still imply those
contracts.

Retain `wake_ecma_vm`, `wake_js_runtime`, `wake_test`, `wake_test_host`, `wake_app`, `wake test`,
`runTests()` and `TestContext`, add `wake_test_contract` as the sole result and wire owner, and
migrate their internals and experimental public types atomically to the ownership in this decision.
Remove the direct `wake_app -> wake_test` dependency; the app and shells exchange only contract
values with the separately packaged host. Add `wake_test_browser` as the only Chromium/CDP owner.
No deprecated Jest facade, external runner fallback, second result schema, second protocol owner,
second DOM loader or second VM backend remains when the replacement converges.
