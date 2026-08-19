# ADR 0017: Source-located terminal diagnostics

- Status: accepted
- Date: 2026-08-19

## Context

Wake compiler diagnostics already retain source paths, byte spans, and labels, but `wake_app`
serialized only the primary byte range. The development-server event path reduced an entire failed
build to a message string, and the Rust and npm terminal edges therefore could not display a source
line or line number consistently.

## Decision

`wake_app` owns the public, serializable diagnostic location. It resolves diagnostic paths against
the prepared project root, reads each source path at most once per diagnostic batch, and attaches a
one-based line/column range, exact source line, and primary label to `DiagnosticInfo`.

`wake_dev_server` forwards compiler diagnostics as structured batches. `wake_app` materializes each
batch before exposing individual Rust or Node development-server diagnostic events. Rust and npm
terminal edges render the resulting DTO according to the shared
`fixtures/terminal-diagnostic-contract.json` contract. The browser overlay remains a separate text
consumer and is not the terminal event protocol.

## Invariants

- Existing `start` and `end` byte offsets remain unchanged.
- `line`, `column`, `endLine`, and `endColumn` are one-based; the end position is exclusive.
- Invalid spans, missing files, pathless diagnostics, and non-source failures omit `location` rather
  than inventing a line number.
- JSON and plain output contain no ANSI escapes; colored output strips to the exact plain shape.
- Rust and npm code frames use the same Unicode-width and four-column tab expansion rules.
- Static, incremental, development, and production build diagnostics use the same DTO.

## Evidence

- `wake_app` tests execute failed static and development builds and assert line 3 source locations.
- Rust and npm terminal tests consume the same machine-readable code-frame fixture.
- CLI integration tests execute a failed build and assert the numbered source line and caret.

## Consequences

The Node `Diagnostic` interface gains an optional `location` object. The serialized native
development-server event now contains `diagnostic` instead of a message-only payload, while the
public Node `diagnostic` listener continues to receive one `Diagnostic` argument with more data.
Reading source snapshots adds bounded work only when diagnostics are materialized and is deduplicated
per path within each batch.

## Validation

Run focused `wake_app`, `wake_dev_server`, and `wake_cli` tests; npm terminal tests and type checks;
the architecture check; workspace tests; pack validation; and `git diff --check`.

## Supersedes

None.

## Removal plan

The message-only server event and Node-side synthetic `WAKE_BUILD` diagnostic are removed in the
same change. No compatibility bridge or second terminal diagnostic protocol remains.
