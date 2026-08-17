# Wake architecture baseline

Use this file only as a routing map. Verify every task against current source, manifests, tests, and `engineering/ARCHITECTURE.md`; the baseline is evidence, not a constraint against redesign.

## Stable product edges

- `wake_app` owns shared application behavior for CLI and Node.
- `wake_cli` and `wake_node` are user-facing shells.
- `wake_docs` builds documentation projects on the Wake compilation pipeline.
- `npm/wake` publishes the JavaScript contract and platform binding loader.
- `npm/css` publishes the `@crab-dev/css` contract.

## Layers

| Layer | Current crates | Direction |
| --- | --- | --- |
| Foundation/compiler | `wake_common`, `wake_ecma_*` | Must not depend on orchestration or product edges. |
| Resolution/analysis/assets | `wake_resolver`, `wake_graph`, `wake_css`, `wake_css_in_js`, `wake_html`, `wake_scan`, `wake_tsdoc` | Consume stable compiler or common models; do not own product orchestration. |
| Incremental/orchestration | `wake_turbo`, `wake_cache`, `wake_bundler` | Coordinate stable task values, module graphs, chunks, and artifacts. |
| Product edges | `wake_dev_server`, `wake_docs`, `wake_app`, `wake_cli`, `wake_node` | Adapt shared behavior to HTTP, docs, CLI, and Node surfaces. |

The executable dependency rules live only in `engineering/architecture-boundaries.json`.

## Application data flow

```text
CLI / Node
→ wake_app configuration and lifecycle
→ resolver/load/parse
→ module and binding liveness
→ transform/codegen/chunk
→ JS/CSS/assets/HTML/manifest
→ memory response or atomic output
```

## Documentation data flow

```text
MDX/frontmatter/navigation/Demo/Props
→ generated .wake/docs React project
→ Wake bundler and Docs runtime
→ routes/search/assets
→ static docs-dist
```

## Sources to read by task

- Ownership or crate changes: `engineering/ARCHITECTURE.md`, affected `Cargo.toml` files, `cargo metadata --no-deps`.
- Compiler/bundler/cache/HMR: `engineering/DESIGN.md`, `engineering/TESTING.md`, source tests for the affected stage.
- CSS-in-JS: `engineering/CRAB_CSS.md`, `npm/css`, `wake_css_in_js`, bundler CSS integration tests.
- Docs: `docs/navigation.toml`, `wake_docs`, `scripts/check-docs.mjs`, public Docs pages.
- Node/npm/release: `npm/wake`, root manifests, version/pack/PnP scripts, CI and release workflows.
- Performance: `engineering/PERFORMANCE.md`; never use an unrecorded timing as a design fact.
