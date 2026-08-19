# ADR 0016: Interactive terminal console contract

- Status: accepted
- Date: 2026-08-19

## Context

Wake's Rust CLI and npm CLI independently implemented a full-screen dashboard. Both entered raw and alternate-screen modes, but only recognized single-key quit, clear, and scrolling controls. They had no editable input, paste protocol, screen selection, or clipboard ownership. This prevented the TUI from behaving like a modern interactive console and made the two public CLI surfaces vulnerable to behavioral drift.

The build and development-server lifecycle is already shared through `wake_app`. Terminal presentation and host operating-system effects are product-edge concerns and must not move into the compiler, bundler, or application-service layers.

## Decision

The Rust and npm CLI edges own equivalent terminal interaction state machines: a Unicode-aware single-line editor, bounded in-memory history, cell-based screen selection, clipboard and URL-opener adapters, and complete terminal-mode restoration. The supported commands are `help`, `clear`, `open`, and `quit`; a leading slash is optional and submitted `q` aliases `quit`.

`fixtures/terminal-console-contract.json` is the machine-readable command contract consumed by both test suites. Service events and lifecycle remain owned by `wake_app`; the terminal layer only reads the already-produced endpoint and requests the existing stop path.

## Invariants

- `--ui plain` never captures input, mouse events, paste, or clipboard state and emits no TUI escape sequences.
- Rust and npm parse the shared command fixture identically.
- Selection coordinates use rendered terminal cells, including wide and combined Unicode graphemes.
- `open` can only use the endpoint produced by the active Wake service and cannot execute user-provided URLs.
- Clipboard and opener failures are diagnostic UI events and do not alter build or server state.
- Raw mode, alternate screen, mouse capture, bracketed paste, and cursor visibility are restored on every exit path.
- User input and command history are process-local and are never persisted.

## Evidence

- `crates/wake_cli/src/console.rs` and `crates/wake_cli/src/dashboard.rs` implement and test the Rust interaction model.
- `npm/wake/bin/console.mjs`, `npm/wake/bin/terminal.mjs`, and their tests implement the npm model and streaming terminal decoder.
- `fixtures/terminal-console-contract.json` gates command parity.

## Consequences

Typing `q` or `c` no longer performs an immediate action; text is edited in the command line and actions occur on Enter. Ctrl-C remains an immediate interrupt. The npm package and Rust CLI gain platform clipboard, Unicode-width, and URL-opener dependencies. No Node API, Rust application API, configuration, cache, or persistent artifact changes.

## Validation

Run focused Rust and npm terminal tests, `npm run architecture:check`, npm pack validation, workspace tests, and `git diff --check`. Exercise selection, paste, commands, resize, and restoration in Windows Terminal; release CI covers supported Linux and macOS builds.

## Supersedes

None.

## Removal plan

The former direct `q`/`c` handlers, footer contract, and complete-chunk npm key parser are removed in the same change. No compatibility bridge or duplicate interaction path remains.
