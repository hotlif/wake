# Risk-matched validation

Run focused checks while iterating and the broadest applicable gate before delivery. Report commands not run and why.

| Change | Minimum evidence | Broader gate when applicable |
| --- | --- | --- |
| Crate ownership/dependency | `npm run architecture:check` plus affected crate tests | workspace clippy/test |
| Lexer/parser/semantic/codegen | affected crate tests and snapshots | `cargo test --workspace`, fuzz smoke |
| Arena or unsafe lifecycle | affected tests | Miri targets from `engineering/TESTING.md` |
| `wake_turbo` concurrency | focused engine tests | Loom single-flight gate |
| Bundler/runtime/chunk | focused Rust regression plus execute generated output | full `wake_bundler` tests and fixture build |
| Cache/HMR | same-session mutation and cold/warm equivalence | browser HMR or persistent-cache fixture |
| Node/npm API | Node tests and TypeScript checks | native build, startup, pack, clean install |
| PnP/components | focused resolver/build test | `npm run pnp:components:check` |
| Wake Docs content | `npm run docs:check` | `cargo test -p wake_docs`, `npm run docs:build` |
| Docs runtime/visual | production build, console and DOM checks | 1440/1024/390, light/dark, keyboard, reduced motion |
| Crab CSS package/compiler | package runtime/type tests plus compiler tests | bundler CSS tests, pack smoke, fixture build |
| Performance claim | correctness checksum plus reproducible measurements | Criterion/2k-module comparison with recorded environment |
| Release contract | versions and pack checks | platform/PnP/registry smoke from release workflow |

## Validation rules

- Use string assertions for local emitted shape only.
- Execute output for module lifecycle, cycle, interop, chunk loading, CSS cascade, and runtime claims.
- Test changed cache inputs in one long-lived session; a fresh process can hide stale-query defects.
- Compare cold and optimized paths before claiming a performance or cache improvement.
- Keep performance results separate from correctness gates.
- Run `git diff --check` and inspect the final diff for accidental scope expansion, dual paths, and stale documentation.
