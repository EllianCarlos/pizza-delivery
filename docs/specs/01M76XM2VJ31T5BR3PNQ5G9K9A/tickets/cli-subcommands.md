---
id: cli-subcommands
blocked_by: [handler-wiring]
---

# CLI subcommands

In `main.rs` add `inbox`, `events`, `artifacts` subcommands with flags from the spec (`--job`, `--stage`, `--kind`, `--severity`, `--actor`, `--path`, `--min-confidence`, `--since`, `--until`, `-q/--search`, `--limit`, `--json`). Parse `--since/--until` as RFC 3339 or relative durations (`2h`, `3d`). Print one line per item, or the Response JSON with `--json`. Errors use existing exit codes.

Test first: end-to-end tests in `crates/pizza-delivery/tests` with the harness: seed events, run each subcommand with flags, assert `--json` output, exit codes 2 and 3, and that `status` output is unchanged.
