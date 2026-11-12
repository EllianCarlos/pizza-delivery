---
id: tui-form
blocked_by: []
---

# TUI new-job form with collapsed Additional Parameters

Add a `tui` subcommand with a UI-independent form state, pure `update` reducer and `to_request` mapping (omit `options` when closed or empty). Check `cargo build --offline` with ratatui and crossterm (both are in the local cargo cache); if they cannot be built, implement the line-based fallback over the same state machine and record it. Send through `client::send`. Tests: reducer/focus/editing, `to_request` parity with the CLI mapping, and `TestBackend` rendering of the closed and open states.
