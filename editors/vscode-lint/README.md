# Wake Lint for VS Code

This workspace extension starts `wake-lint-language-server` and publishes native Wake lint
diagnostics for JavaScript, TypeScript and JSX/TSX documents. Set `wakeLint.serverPath` when the
server is not on `PATH`; the server reads the workspace root and receives unsaved document snapshots
through standard LSP document synchronization. Safe native fixes are exposed as preferred LSP
quick-fix code actions, so VS Code's lightbulb can apply the same edits as `wake lint --fix`.
