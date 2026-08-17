# Wake invariant map

Select the relevant sections for the task, then verify them against current implementation and tests.

## Universal

- Correctness precedes diagnostics, determinism, incremental reuse, and throughput.
- A public behavior has one authoritative owner and one contract source.
- Diagnostics preserve source identity and actionable locations.
- Paths, names, hashes, and ordering are deterministic across supported platforms.
- Optimization has a conservative path whose behavior can be compared.

## Compiler and semantic analysis

- Binding-sensitive behavior uses semantic identity, not spelling alone.
- AST/arena references and process-local atoms do not cross persistence boundaries.
- Transform and codegen preserve spans required by diagnostics and source maps.
- Unsupported syntax fails explicitly or follows a documented conservative path; it does not silently delete behavior.

## Bundler and runtime

- Scan, Link, and Emit ownership remains explicit.
- Tree shaking follows binding liveness; global side effects remain intentional and testable.
- ESM/CJS cycles, top-level await, concat, and dynamic chunks preserve single execution and export identity.
- Critical regressions execute generated output; string assertions alone prove only code shape.

## Incremental cache and HMR

- Task identity contains every non-reactive semantic input captured by codegen or emit.
- Output fingerprints include every artifact that can change downstream behavior.
- Cold, persistent-cache, warm-session, development, and production builds are semantically equivalent.
- HMR updates stable ownership slots and removes stale state; it does not accumulate anonymous artifacts.
- Cache DTOs contain stable serializable data, never arena pointers or process-local IDs.

## Node/npm and release

- CLI and Node user behavior converge through `wake_app` unless an accepted ADR changes ownership.
- ESM, CommonJS, and TypeScript declarations describe the same public surface.
- Workspace, main package, CSS package, and platform package versions stay synchronized where required.
- Pack validation uses clean-install artifacts, not only the working tree.
- A breaking switch updates package code, declarations, consumers, tests, docs, and release workflows atomically.

## Wake Docs

- File paths own routes; `navigation.toml` owns visible hierarchy and order.
- Frontmatter owns page metadata, not navigation placement.
- MDX, Demo, Props extraction, runtime, search, and static shells agree on page identity.
- Public Docs explain user contracts; implementation, PnP bridges, and release internals remain in `engineering/`.
- Browser-visible changes require console, accessibility, responsive, and theme validation proportionate to risk.

## Crab CSS

- `@crab-dev/css` is the public CSS-in-JS contract.
- Static evaluation never executes arbitrary user code or silently drops an unresolved style.
- Dynamic values cross the explicit CSS custom-property boundary.
- Style identity is stable across unrelated insertion and supported paths/platforms.
- CSS order, global effects, keyframes, URLs, chunks, cache, and HMR are evaluated as one artifact data flow.
