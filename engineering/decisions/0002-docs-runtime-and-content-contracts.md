# ADR 0002: Separate Wake Docs runtime surfaces and enforce content contracts

- Status: accepted
- Date: 2026-08-14

## Context

Wake Docs serves two products from one generator: the public documentation site and the component workbench. The previous generated entry imported both products unconditionally, so a normal documentation build bundled the component explorer and every installed `@crab-dev/rc-*` package. Navigation expansion also mixed route-derived state with user preference, while content checks only required headings and allowed configuration reference pages to drift from `wake_config`.

The public site build measured 2,306 modules, about 2.60 MB of JavaScript and 179 KB of CSS before this decision. Browser inspection also showed that visiting pages accumulated persisted expanded sections.

## Decision

Generate a mode-specific runtime entry. Site mode owns only the documentation application and its base styles. Components mode owns the component workbench, component state and component styles. Shared code remains in `app.tsx`, but neither mode imports the other product surface.

Treat the active navigation section as route-derived state. Persist only sections the user explicitly toggles. Group overview pages live directly under their navigation group instead of being repeated as the first page of a child section.

Make page `kind` an executable content contract in `scripts/check-docs.mjs`. Tutorials require an outcome, runnable code, verification, common errors and a next step. Guides require a verification or measurement section and a next step. Overviews require a primary task entry and multiple task-oriented sections.

Use the public Rust configuration structs as the source for reference coverage. The documentation check extracts public fields from `wake_config` and fails if their assigned reference page omits a field.

## Invariants

- Site mode must not generate or import the component workbench runtime.
- Components mode must include the workbench and its state and style resources.
- The current section is open without being written into the user expansion preference.
- Navigation order and hierarchy come only from `docs/navigation.toml`.
- A page kind determines its minimum evidence and completion structure.
- Every public `wake_config` field appears in one authoritative configuration reference page.
- Invalid configuration fails with a diagnostic; only a missing file uses defaults.

## Evidence

- `crates/wake_docs/runtime/site-entry.tsx`
- `crates/wake_docs/runtime/components-entry.tsx`
- `crates/wake_docs/src/lib.rs`
- `crates/wake_docs/runtime/app.tsx`
- `scripts/check-docs.mjs`
- `docs/reference/configuration/`
- Production build output and browser checks recorded in the implementation task.

## Consequences

The two runtime products can evolve independently and public documentation no longer pays for the component catalog. Adding a page or public configuration field now requires satisfying an explicit check. Content authors receive earlier failures, at the cost of keeping tutorials and guides structurally complete.

The generated entry filename is an internal implementation detail and changes between modes. Public Wake Docs commands, routes, Frontmatter and MDX APIs do not change.

## Validation

- `cargo test -p wake_docs`
- `npm run docs:check`
- `npm run docs:build`
- Inspect the site build manifest and asset sizes; no `@crab-dev/rc-*` package may enter site mode.
- Navigate multiple sections in a real browser and confirm only explicit toggles persist.
- Verify desktop and mobile layouts, deep links, search and the browser console.

## Supersedes

None.

## Removal plan

The shared `runtime/entry.tsx` path is removed immediately. No compatibility wrapper, dual entry selection inside the browser, or legacy expansion storage key remains.
