# Web and TUI submit forms with "> Additional Parameters"

## Problem

The earlier job (spec `01M76XCVTRB3YP805PJW715BW4`) added `TriageOptions` to the core API, ledger, store, CLI and `--dry-run`, and only documented the web/TUI form as a contract. The requester now wants the web and TUI forms implemented. Neither exists in this repo: there is no `web/` directory, no TUI, and the daemon (`crates/pizza-delivery/src/server.rs`) only accepts line-JSON on a Unix socket, so a browser cannot reach it. Without these surfaces the "Additional Parameters" pattern (a collapsed, ignorable group under the prompt) is not usable outside the CLI.

## Solution

Build the smallest slice per surface that lets a user write a prompt, optionally expand "> Additional Parameters", fill the five triage options, preview with dry-run and submit. No job board, inbox, detail view or SSE; those stay in `docs/designs/pizza-delivery.md` as future work.

1. **Daemon HTTP adapter** (new module in `crates/pizza-delivery`, `std::net`, no new HTTP dependency). Opt-in via config key `http_listen` (default unset; example `127.0.0.1:7878`). Routes:
   - `GET /` serves the embedded web form (HTML + JS, `include_str!`).
   - `POST /api/submit` and `POST /api/dry-run` take JSON `{prompt, repo?, priority?, options?}`, are mapped to `Request::Submit` / `Request::Triage` and dispatched through the same code path as the socket (`handle` / `dry_run`), and return the `Response` JSON. `Response::Error` with kind `usage` returns HTTP 400, other errors 500, success 200.
   - Safety: bind only to loopback addresses (a non-loopback `http_listen` is a config error); reject requests whose `Host` is not a loopback name/IP or whose `Origin` (when present) is not the same loopback origin; require `Content-Type: application/json` on POST; cap body at 64 KiB; unknown route 404, wrong method 405.
2. **Web form** (`web/`, plain HTML + TypeScript-free JS module, no npm dependencies, tested with `bun test`). A pure function `buildSubmitBody(fields)` turns form fields into the JSON body: it omits `options` entirely when the expander is collapsed or every option is empty, omits unset fields, trims `context_hint`, parses `max_stages` to an integer and splits risk flags. The page has a `<details>`-style expander labelled "Additional Parameters" (collapsed by default, rendered with a ">" marker) holding the five fields in order: template, report only, max stages, context hint, risk flags. Buttons: "Preview triage" (dry-run) and "Submit". Server validation errors are shown inline.
3. **TUI form** (`pizza-delivery tui` subcommand, ratatui + crossterm if they build offline, otherwise a line-based prompt fallback behind the same state machine). A UI-independent form state with a pure reducer `update(state, key) -> state` (fields: prompt, repo, expander open/closed, the five option fields, focus) and a function `to_request(state) -> Request` with the same omission rules as the web. Keys: Tab/Shift-Tab move focus, Enter on the expander row or Right/Left toggles it, Ctrl-P previews (dry-run), Ctrl-S submits, Esc quits. Requests go through the existing `client::send` over the Unix socket. Result or error is shown in a status line.
4. **Docs**: replace "documented, not built" wording in `docs/designs/pizza-delivery.md` and `README.md` with the built behaviour, the HTTP routes, the `http_listen` setting and the loopback safety rules; update the previous spec's UI-contract line to point here.

Behavior with no options stays byte-identical to today on the socket path, ledger and CLI. Validation is not duplicated in the UIs beyond input parsing; the server remains the authority.

## Acceptance criteria

- With `http_listen` unset, the daemon opens no TCP listener. With a non-loopback address it fails to start with a config error.
- `POST /api/submit` with `{"prompt":"x"}` creates a job exactly as `Request::Submit` without `options`; the stored `job-submitted` event has no `options` key.
- `POST /api/submit` with `options` persists them as in the CLI path and returns the `Submitted` response; `POST /api/dry-run` returns `Triaged` reflecting the options and enqueues nothing.
- A usage error (for example `max_stages: 0`, blank `context_hint`, empty prompt) returns HTTP 400 with the `Response::Error` body.
- Requests with a non-loopback `Host`, a foreign `Origin`, a non-JSON content type, a body over 64 KiB, an unknown path or a wrong method are rejected (403, 403, 415, 413, 404, 405) and change no state.
- `GET /` returns the form HTML containing a collapsed "Additional Parameters" expander.
- `buildSubmitBody` omits `options` when collapsed or when all fields are empty; with fields filled it emits only those fields with correct types; `bun test` passes.
- TUI reducer: the expander starts closed; Tab skips option fields while closed and visits them in order (template, report only, max stages, context, risk) when open; typing edits the focused field; `to_request` omits `options` when closed or all empty and otherwise matches the CLI mapping for the same inputs.
- TUI renders the closed and open states against a test backend (or, in the fallback, produces the same prompts in order) and `pizza-delivery tui` exits cleanly on Esc without sending a request.
- All existing tests pass unchanged.
- Docs no longer say the web/TUI forms are "documented, not built"; `README.md` shows how to enable HTTP and start the TUI.

## Seams

- HTTP request parsing and routing as a pure function from raw request bytes to `(status, body)` over a fake `Store`/`Env`: unit tests in the new HTTP module, covering safety rejections without opening a socket.
- HTTP adapter end to end on a loopback ephemeral port against a real daemon thread with the test harness: new `crates/pizza-delivery/tests/http.rs` using `tests/harness/mod.rs` (submit, dry-run, usage error, restart persistence not re-tested).
- Config parsing of `http_listen` and the loopback check: unit tests in `config.rs`.
- Web form-to-JSON function `buildSubmitBody`: `bun test` in `web/`.
- TUI form reducer and `to_request`: pure unit tests, no terminal.
- TUI rendering: ratatui `TestBackend` buffer assertions (only if ratatui builds offline; otherwise the fallback's prompt sequence is tested).
- Socket path unchanged: existing `tests/walking_skeleton.rs` and `tests/triage.rs` act as regression seams.
- Not covered: real browser rendering and real terminal behavior; verified manually.
