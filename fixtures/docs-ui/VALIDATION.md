# Rendering interface validation

Validated on Windows x64 on 2026-09-10 using the CLI built from this working tree.

## Automated evidence

- `cargo test -p wake_config -p wake_docs -p wake_app --lib`: 271 passed,
  2 existing ignored tests. The development HTTP regression covers UI edits,
  complete-graph selection, diagnostics, retention of the last valid bundle and recovery.
- Registry/state tests: 5 passed. React DOM tests: 3 passed, including optional
  fallback, intentional null rendering, shared Context and visible rendering errors.
- Public npm declarations, runtime TypeScript and fixture component TypeScript checks passed.
- `check-npm-packs.mjs` for `npm/wake` passed: 31 files; the packed `./docs`
  export was also imported and exercised from an extracted consumer package.
- Affected crate Clippy and formatting checks passed.
- `docs:check` passed: 53 routes, 118 Markdown files, 8 fixture pages; its 9 search tests passed.
- `architecture:test`: 55 passed. The complete `architecture:check`, including ADR
  relationships and dependency provenance, passed after temporary-directory housekeeping.
- Default, partial and full fixture production builds passed under `/handbook/`.
  The main Wake documentation production build also passed (53 routes).

## Browser evidence

The default, partial and full examples were exercised in the real in-app Chromium
browser at desktop and 390 × 844 mobile dimensions.

- Default navigation, search with Control+K, Escape dismissal and trigger focus worked.
  Navigation expansion survived reload; deep links and browser history navigation worked.
- Custom search and mobile navigation reused Wake routes and restored focus to the
  destination H1 after the Crab Dialog close transition.
- Partial and full examples rendered `shared-context` from the Root Provider in MDX.
  Full coverage rendered no `data-wake-default` nodes; missing routes used custom NotFound.
- Inline and Portal probes retained `rgb(19, 71, 113)`, monospace and a 3px border
  in light and dark themes. Root Crab tokens remained absent. Theme persisted on reload.
- Mobile layout had no horizontal document overflow. Default drawer had an opaque
  background and occupied the viewport after its opening animation.
- Demo iframe interaction incremented its counter; source and API filtering worked.
  Fullscreen rendering and Escape dismissal retained a single iframe, including the
  exit transition. The independently configured Preview wrapper remained active.
- No browser error logs were observed in these scenarios.

## Temporary artifacts and validation scope

The initial architecture check reported nine pre-existing binary/archive files in the
unignored root `.tmp` directory. Follow-up housekeeping scoped the ignore rule to
`/.tmp/`, removed this task's obsolete helper scripts and backup, and moved its remaining
type-check configuration into `.tmp/docs-ui/`. The complete architecture check then passed.
Other tasks' files were preserved. The validator still checks tracked files, including
tracked files inside ignored directories; its implementation and source rules are unchanged.

The complete native release consumer/platform matrix was not run for this JavaScript
entry addition. The focused declaration, package-content and packed-export checks above
cover the new entry; release validation remains governed by `engineering/TESTING.md`.
