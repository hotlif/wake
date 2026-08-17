---
name: architect-wake
description: Architecture-first evolution for the Wake repository. Use when designing or implementing Wake features, refactors, bundler/runtime/cache/HMR changes, Rust crate boundaries, Node/npm APIs, Wake Docs routing, @crab-dev/css compiler contracts, performance work, or complex defects. Do not implicitly use for copy-only edits, formatting-only changes, or work outside Wake.
---

# Architect Wake

Treat architecture as a falsifiable model that evolves with evidence. Make implementation serve the target architecture; do not make the target architecture imitate convenient existing code.

## Start from the system

1. Confirm the repository root, worktree state, affected product capability, and current public contract.
2. Classify the task before searching for edit locations:
   - **L0 local maintenance**: copy, formatting, or mechanical version synchronization with no behavior change. State `No architecture impact`; do not create an ADR.
   - **L1 behavioral change**: change one subsystem without moving ownership or changing a cross-layer contract. Publish a concise Architecture Brief.
   - **L2 architecture evolution**: change ownership, dependencies, data flow, cache identity, persistent formats, public contracts, or remove an old path. Publish a complete Architecture Brief and add or update an ADR.
3. Treat cache, HMR, bundler runtime, persistence, Node/npm APIs, Docs routing, and CSS compiler contracts as L2 unless evidence proves the change is mechanical.
4. Read [wake-architecture.md](references/wake-architecture.md) to locate the capability. Read only the domain sections needed from [invariants.md](references/invariants.md) and [validation.md](references/validation.md). For L1/L2, read [architecture-loop.md](references/architecture-loop.md) completely.

## Publish the architecture before editing

For L1/L2, send `Architecture Brief v1` before modifying tracked files. Include:

- product goal and measurable success;
- current architecture with source, test, or manifest evidence;
- target architecture and ownership;
- facts, inferences, decisions, and unverified hypotheses as separate lists;
- components, dependency direction, and end-to-end data flow;
- correctness, diagnostics, determinism, incremental, and cross-platform invariants;
- public interface, artifact, configuration, and migration impact;
- structures to delete or replace;
- experiments and validation gates;
- convergence conditions for this task.

Expose reviewable decisions and evidence, not private chain-of-thought. Rank tradeoffs in this order: correctness, diagnostics, cross-platform determinism, incremental work avoidance, throughput, implementation convenience.

## Evolve with evidence

1. Audit the current implementation as evidence, not as the desired design.
2. Test high-risk assumptions with the smallest representative experiment before committing to a broad implementation.
3. If evidence invalidates a hypothesis, stop at a safe boundary and publish `Architecture Brief vN+1`:
   - identify the invalidated hypothesis and affected decisions;
   - revise the target model and vertical slices;
   - update the ADR when the durable decision changes;
   - continue from the revised architecture without hiding the contradiction behind a compatibility patch.
4. Ask for new authority only when the revised architecture materially expands user-approved scope, affects user data, or requires an irreversible external action.

## Implement a complete vertical slice

- Choose the smallest complete slice that reaches the target architecture, not the smallest diff and not the largest rewrite.
- Update the model, implementation, callers, public types, diagnostics, tests, documentation, and release configuration together when they share a contract.
- Preserve unrelated worktree changes. Never use destructive Git operations to simplify migration.
- Default to an atomic repository-wide switch. Do not add deprecated wrappers, dual writes, compatibility parsers, old routes, or permanent feature flags unless compatibility is an explicit requirement.
- When a bridge is necessary, record its owner, scope, removal condition, and latest removal milestone in the ADR.
- Delete superseded paths in the same slice. Do not finish with an undocumented second implementation or second source of truth.

## Enforce architecture, not prose

- Run `npm run architecture:check` whenever crate boundaries or ADRs may be affected.
- Update `engineering/architecture-boundaries.json` only with a referenced `proposed` or `accepted` ADR.
- Create a new ADR when a durable decision changes. Mark the old ADR `superseded`; never rewrite history to make the old decision appear current.
- Add a machine gate for every deterministic invariant that would otherwise depend on reviewers remembering it.
- Do not duplicate machine-readable rules in narrative documents. Explain intent and link to the rule source.

## Prove convergence

Before declaring completion, audit all applicable conditions:

- one clear owner for the capability;
- dependency direction matches the target model;
- one source of truth for core data;
- cache identity includes every semantic input;
- cold, warm, development, and production semantics agree;
- failures identify the user or caller source;
- the new contract fully replaces the old contract;
- temporary bridges and duplicate implementations are removed;
- each new abstraction serves a real data flow or multiple consumers;
- hypotheses are supported by tests, executed artifacts, browser behavior, or reproducible benchmarks;
- introduced complexity is lower than the complexity removed.

For every non-applicable condition, state why. Run the risk-matched gates in [validation.md](references/validation.md), re-audit the diff for new cycles and dual paths, and report completed evidence, skipped gates, deletion results, and remaining unverified risks.
