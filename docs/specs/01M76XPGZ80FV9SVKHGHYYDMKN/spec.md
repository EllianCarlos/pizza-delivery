# Web and TUI submit forms with "> Additional Parameters"

## Problem

The earlier job (spec `01M76XCVTRB3YP805PJW715BW4`) added `TriageOptions` to the core API, ledger, store, CLI and `--dry-run`, and only documented the web/TUI form as a contract. The requester wants the web and TUI submit forms to expose the collapsed "Additional Parameters" group.

## Solution

Built on top of the daemon's existing HTTP adapter, Cloudscape web UI and ratatui TUI (not a separate adapter).

1. **HTTP**: `POST /api/jobs` accepts an optional `options` object (`TriageOptions`) and forwards it in `Request::Submit`. Validation is the handler's, shared with the socket path; usage errors are HTTP 400.
2. **Web**: the Submit modal in `web/src/App.tsx` has an `ExpandableSection` "Additional Parameters", collapsed by default, with the five fields in order: template, report only, max stages, context hint, risk flags. `web/src/options.ts` (`buildOptions`) omits `options` when collapsed or empty, trims, parses the count and splits risk flags.
3. **TUI**: the `n` new-job mode is a form (`NewJobForm` in `crates/pizza-delivery-core/src/tui.rs`) with prompt, a collapsed "> Additional Parameters" row and, when open, the five options. `Command::Submit` carries `options`, omitted when closed or empty.
4. **Docs**: design doc, README and the previous spec's UI-contract line say the forms are built.

Behaviour with no options is unchanged on every path.

## Acceptance criteria

- `POST /api/jobs` with `options` persists them like the CLI; invalid options return 400 `usage`; without `options` nothing changes.
- `buildOptions` omits `options` when collapsed or empty and emits typed, trimmed fields otherwise (`bun test`).
- The TUI expander starts closed; Tab skips the options while closed and visits them in order when open; `Command::Submit` has `options: None` when closed or empty; the closed and open states render on a `TestBackend`.
- All existing tests pass.

## Seams

- `crates/pizza-delivery/tests/http.rs` for the HTTP wiring.
- `web/src/options.test.ts` and `web/src/api.test.ts` for the web body.
- Pure `Model::handle_key` tests in the core `tui.rs` and `TestBackend` frames in `crates/pizza-delivery/src/tui/ui.rs`.
- Not covered: real browser and terminal behaviour.
