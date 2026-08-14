# Wake architecture decision records

This directory preserves durable architecture decisions. Source and tests remain the implementation facts; ADRs explain why an architecture was selected, which invariants it protects, and how it may be replaced.

## Lifecycle

Allowed status values:

- `proposed`: under validation;
- `accepted`: adopted and reflected in the repository;
- `superseded`: replaced by a newer ADR;
- `rejected`: evaluated but not adopted.

Create a new numbered ADR from `0000-template.md`. Do not rewrite a previous decision after its context changes. A replacement ADR lists the old record under `Supersedes`; the old record becomes `superseded` and adds a `Superseded by` link.

`npm run architecture:check` validates numbering, status, required sections, links, and references from the machine-readable boundary policy.
