# ADR 0001: Make architecture evolution an executable loop

- Status: accepted
- Date: 2026-08-14

## Context

Wake documents its 25-crate layering, build data flows, testing matrix, CSS design, and product boundaries. Those documents do not create durable decision history and do not automatically reject a new reverse dependency. Architecture work can therefore degrade into implementation-first patches or prose that drifts from the repository.

## Decision

Adopt an architecture evolution loop consisting of a repository-scoped `architect-wake` Skill, versioned Architecture Briefs, ADRs, a machine-readable crate boundary policy, and a CI architecture check.

Treat architecture as a falsifiable model. Evidence may revise the target model during implementation. Implement the smallest complete vertical slice, remove the superseded path, and re-audit convergence. Default to a breaking atomic switch unless compatibility is an explicit product requirement.

## Invariants

- Architecture decisions precede implementation details for behavioral and structural changes.
- Current code is evidence, not an obligation to retain an incorrect boundary.
- Deterministic boundaries are machine checked from Cargo metadata.
- Durable decisions retain history when superseded.
- A completed architecture slice leaves one owner and one source of truth.
- Destructive data operations and unrelated worktree changes remain outside the breaking-change policy.

## Evidence

- `engineering/ARCHITECTURE.md` defines dependency direction but no script previously enforced it.
- `engineering/TESTING.md` defines risk-specific gates but no architecture task classification.
- `cargo metadata --no-deps` provides the current workspace crate and dependency graph.
- The official Codex Skill location for repository-wide workflows is `.agents/skills`.

## Consequences

Architecture-affecting work gains mandatory reviewable decisions, falsifiable hypotheses, deletion audits, and CI feedback. Adding or reclassifying a crate requires updating the boundary policy and referencing a proposed or accepted ADR. Some changes become intentionally larger because all repository consumers switch together.

## Validation

- Validate the Skill with the standard Skill validator.
- Run architecture checker tests with invalid dependency, unknown crate, ADR status, and supersedes fixtures.
- Run `npm run architecture:check` against the actual workspace.
- Run the architecture job independently in CI.

## Supersedes

None.

## Removal plan

No compatibility bridge is introduced. If the architecture workflow is replaced, a new ADR must supersede this record and remove the Skill, policy, checker, and CI job together.
