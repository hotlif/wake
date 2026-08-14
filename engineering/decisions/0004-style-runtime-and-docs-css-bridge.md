# ADR 0004: Make style runtime ownership deterministic and bound the Docs CSS bridge

- Status: superseded
- Date: 2026-08-14
- Superseded by: [ADR 0005](0005-chunk-owned-style-artifacts.md)

## Context

Development CSS-in-JS previously embedded a namespace derived from process ID, wall-clock time and a
process-local counter. That kept two bundles from sharing a document-global style slot, but made
otherwise identical build output differ across processes and forced CSS codegen task identity to
change on every cold bundler instance.

Production Wake Docs routes may contain extracted global or component styles. Wake does not yet emit
an ordered entry/chunk-to-CSS manifest or load CSS before executing a dynamic chunk. Docs therefore
disabled code splitting for correctness, but the bridge had no durable owner or removal milestone.

## Decision

Use the bundle runtime's `__wake_require__` function object as the development style owner. Store
owner registries in a document-scoped `WeakMap`, then address styles inside one owner by deterministic
module ID. Build-time output contains no process identity, time or random value.

Keep the production Wake Docs single-chunk bridge until the bundler owns an atomic `StyleArtifact`
flow with an ordered entry/chunk-to-CSS manifest and a loader that completes relevant CSS before a
dynamic module executes. The bridge is scoped only to `wake_app::build_docs_with_mode`; ordinary
application production builds retain code splitting.

`wake_bundler` owns delivery of the replacement artifact protocol. `wake_app` owns deletion of the
bridge once that protocol is available. The bridge must be removed before `@crab-dev/css` 0.2.0 or
before Wake Docs advertises page-level code splitting, whichever comes first.

## Invariants

- Identical source graphs and options produce byte-identical development JavaScript across processes.
- Independent bundle runtimes cannot overwrite each other's style slots in one document.
- Dynamic chunks share the entry runtime's style owner.
- Re-executing a module upserts or removes its stable slot without accumulating anonymous styles.
- Production Docs never executes a route chunk before all CSS artifacts that affect it are active.
- While the bridge exists, a production Docs build emits exactly one JavaScript chunk with a content hash.

## Evidence

- `crates/wake_bundler/src/incremental.rs`
- `crates/wake_bundler/src/tests.rs`
- `crates/wake_app/src/lib.rs`
- `engineering/CRAB_CSS.md`
- Executed Node DOM-mock regression for two independent bundle runtimes.

## Consequences

Development output becomes reproducible and CSS codegen no longer carries a per-process input. The
runtime adds one document `WeakMap` lookup per executing styled module and releases owner registries
when their require function becomes unreachable.

Wake Docs retains a larger initial JavaScript artifact while the correctness bridge exists. This is
an explicit bounded cost, not the target performance architecture.

## Validation

- Build identical development inputs with independent bundlers and compare JavaScript bytes.
- Execute two bundles with identical module IDs against one document mock and verify two style nodes.
- Run full `wake_bundler` tests, including dynamic chunk execution.
- Build a multi-route Docs fixture and assert one content-hashed JavaScript chunk.
- Run `npm run docs:build`, `npm run docs:check`, CSS package tests and type checks.

## Supersedes

None.

## Superseded by

[ADR 0005](0005-chunk-owned-style-artifacts.md) replaces the production single-chunk bridge with
chunk-owned style artifacts.

## Removal plan

Introduce a serializable `StyleArtifact` that includes CSS, ordering and source identity; attach the
artifact to chunk planning; emit an ordered entry/chunk-to-CSS manifest; teach browser loading and
HTML generation to activate the required CSS; add cold/warm, dynamic import, HMR and browser cascade
tests; then delete `disable_code_splitting()` from Docs and replace the single-chunk bridge test with
page-level chunk/CSS execution tests. No dual CSS loader remains after the switch.
