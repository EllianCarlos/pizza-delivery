# Additional Parameters for job creation (triage options)

## Problem

Job creation takes a prompt, an optional repo and an optional priority. Triage (`classify`, `plan_for`, `risk_flags` in `crates/pizza-delivery-core/src/triage.rs`) decides the plan template, stages and risk flags purely from keyword heuristics on the prompt. The submitter cannot say "this is a bug fix", "stop before shipping" or "keep it to N stages" without rewording the prompt, and cannot add context that helps triage. The wanted UX is an "Additional Parameters" group, collapsed and ignorable, whose every option sharpens triage. No web UI or TUI exists in this repo, and the phrase "Additional Parameters" appears nowhere in it, so the contract (API, ledger, CLI) is specified here and the visual form is documented only.

## Solution

Introduce `TriageOptions`, a struct in `pizza-delivery-core` where every field is optional. All unset means today's behaviour exactly.

Fields:
- `template`: `full-feature | bug-fix | investigate`. Overrides `classify`.
- `report_only`: bool. Drops the `implement`, `review` and `ship` stages from the plan, so the job ends with findings and no PR.
- `max_stages`: integer 1..=`DEFAULT_MAX_STAGES`. The planned stage count is capped at this value. If the cap would drop stages, keep the leading ones.
- `context_hint`: free text, max 2000 characters, trimmed. Appended to the restated task as a `Hint:` line and recorded on the job. It does not change template choice.
- `risk_flags`: list of extra flags (kebab-case strings) that the submitter asserts, merged with the heuristic flags without duplicates.

Wire changes:
- `Request::Submit` and `Request::Triage` gain `options: Option<TriageOptions>`, with `serde(default, skip_serializing_if = "Option::is_none")`. Old clients and old ledger events still deserialize.
- `EventPayload::JobSubmitted`, `NewJob` and the projected `Job` carry `options` with the same serde rules. Stored events without the field replay as `None`.
- `triage()` and `conclude()` take the options and apply them. When an option overrides a different heuristic result (for example `template=bug-fix` while keywords say investigate), `conclude` adds a feedback entry `kind: "assumption"`, severity `low`, stating what was overridden. Overrides are hard, not advisory.
- Validation lives in the handler and in the dry-run path. A bad value (empty `context_hint`, `max_stages` of 0 or above the cap, `report_only` with `template=full-feature` is allowed) returns `ErrorKind::Usage`.

CLI: a flag group labelled "Additional Parameters" in `--help` (clap `help_heading`): `--template <t>`, `--report-only`, `--max-stages <n>`, `--context <text>`, repeated `--risk <flag>`. None are required. `--dry-run` shows the effect of the options.

UI contract (documented, not built): a web or TUI form shows a collapsed "> Additional Parameters" expander under the prompt field, listing these same five fields in this order. Collapsed means the request carries no `options` key.

## Acceptance criteria

- Submitting with no options produces the same events, plan and JSON as before this change. Existing tests pass unchanged.
- A `Request::Submit` JSON without `options` deserializes. A `job-submitted` event JSON without `options` replays to a `Job` whose `options` is `None`.
- `options` set on submit are persisted on the `job-submitted` event, projected onto `Job`, and survive a daemon restart (SQLite store round trip).
- `template` override replaces the classified template and plan. When it differs from the heuristic result, the outcome has one `assumption` feedback entry naming both.
- `report_only` yields a plan with no `implement`, `review` or `ship` stage, for all three templates.
- `max_stages` truncates the plan to N stages, leaving at least one. `max_stages` of 0 or above `DEFAULT_MAX_STAGES` is rejected with a usage error (exit code 2).
- `context_hint` appears in `Triage.task`. A blank or over-long hint is rejected with a usage error.
- Extra `risk_flags` merge with heuristic flags without duplicates and keep a stable order (heuristic first).
- The `--dry-run` path with options returns a `Triaged` response reflecting them and enqueues nothing.
- `pizza-delivery --help` lists the five flags under an "Additional Parameters" heading. CLI flags map one-to-one onto the request fields.
- `docs/designs/pizza-delivery.md` documents the options and the UI expander contract. `GLOSSARY.md` defines "Triage options".

## Seams

- Core triage functions (`triage`, `conclude`, `plan_for` with options): pure unit tests in `crates/pizza-delivery-core/src/triage.rs`, covering each option, combinations and override feedback.
- API serde: `Request` and `EventPayload` JSON round trips and backward compatibility, unit tests in `api.rs` and `event.rs`.
- Job projection: `fold` in `job.rs` copies options from the submitted event.
- Handler (`handle`) with the in-memory/test `Store`: validation errors and persisted options, via `crates/pizza-delivery/tests/walking_skeleton.rs`.
- Dry-run triage through the server (`server.rs` → `triage.rs`): `crates/pizza-delivery/tests/triage.rs`.
- SQLite store round trip of options: unit test in `sqlite_store.rs`.
- CLI argument parsing to `Request`: clap parse test in `main.rs`. No browser or TUI seam, because none exists.
