# Filter and search for Inbox, Events and Artifacts

## Problem

The ledger records every event, artifact and feedback entry for every job, but the only way to read it is `status` (all jobs, or one job's full timeline). A user cannot ask "show me the high-severity feedback", "all `stage-failed` events for job X since yesterday", or "which artifacts touched `src/auth`". As the ledger grows, finding anything means reading raw JSON.

The repo has no web UI or TUI yet (only core, daemon and CLI), so this job delivers the backend and CLI half. A UI can reuse the same API later.

## Solution

Add pure filter and search functions in `pizza-delivery-core` that run over ledger projections (ADR 0002: the ledger is the source of truth, so no new tables or indexes). Expose them through new daemon `Request`/`Response` variants and three CLI subcommands: `inbox`, `events`, `artifacts`. Existing variants and commands stay unchanged.

Definitions (also added to GLOSSARY.md):
- **Filter**: a set of property constraints narrowing a list. Different properties are ANDed; repeated values of one property are ORed.
- **Search**: a case-insensitive substring match over an item's text fields, ANDed with the filter.

Projections:
- **Inbox item**: a `FeedbackRecorded` event, projected to `{event id, job id, short id, stage id, ts_ms, kind, severity, text, confidence}`. There is no dismissed or reply state in the ledger today, so none is filterable.
- **Artifact item**: an `ArtifactRecorded` event projected to `{event id, job id, short id, stage id, ts_ms, kind, path, sha256}`.
- **Event item**: any `Event`.

Filter properties:
- Events: `job` (id or short id, @last), `stage` (stage id), `kind` (event kind string such as `stage-failed`), `actor`, `since_ms`, `until_ms` (inclusive).
- Artifacts: `job`, `stage`, `kind` (artifact kind), `path` (substring), `since_ms`, `until_ms`.
- Inbox: `job`, `stage`, `kind` (feedback kind), `severity`, `min_confidence`, `since_ms`, `until_ms`.

Search text fields: events = kind plus string fields of the payload (serialized payload text); artifacts = kind, path; inbox = kind, severity, text.

Semantics:
- Empty filter and no query returns everything.
- Results are ordered newest first (`ts_ms` desc, then `seq` desc), with an optional `limit` (default unlimited) applied after filtering.
- Unknown job reference returns the existing NotFound/Ambiguous error. An inverted time range (`since > until`) returns a Usage error. An empty-string query is treated as no query.
- Filtering runs in memory in the handler over the store's existing reads (`list_jobs` + `read_timeline`); no SQL pushdown.

API: new `Request::Inbox`, `Request::Events`, `Request::Artifacts` each carrying its filter struct, `query: Option<String>` and `limit: Option<usize>`; new `Response::InboxItems`, `Response::EventItems`, `Response::ArtifactItems`.

CLI: `pizza-delivery inbox|events|artifacts` with `--job <ref>` (repeatable), `--stage`, `--kind` (repeatable), `--severity` (repeatable, inbox), `--actor` (repeatable, events), `--path` (artifacts), `--min-confidence` (inbox), `--since`/`--until` (RFC 3339 timestamp or relative like `2h`, `3d`), `-q/--search <text>`, `--limit <n>`, `--json`. Human output is one line per item; `--json` prints the Response.

## Acceptance criteria

- Events filter by job, stage, kind, actor and time range; each property alone narrows results correctly.
- Two different properties together return the intersection (AND); two values of the same property return the union (OR).
- Search matches case-insensitively as a substring and is ANDed with filters; a query that matches nothing returns an empty list, not an error.
- Artifacts filter by kind, job, stage, path substring and time range; search matches kind and path.
- Inbox filters by feedback kind, severity, min confidence, job, stage and time range; search matches kind, severity and text. Entries without confidence are excluded by `min_confidence`.
- Results are newest first and `limit` truncates after filtering.
- Unknown or ambiguous job refs map to the existing error kinds and exit code 3; inverted time range exits 2.
- `--json` output round-trips to the Response type; human output has one line per item.
- Existing `status`, `cancel`, `quota`, `submit` behaviour and JSON shapes are unchanged (existing tests still pass).
- GLOSSARY.md defines Filter, Search and Inbox item.
- `cargo test` and `cargo clippy` pass.

## Seams

- Core filter module (unit tests): filter structs and `apply` functions over in-memory `Vec<Event>`; covers AND/OR, search, ordering, limit, time bounds, missing-confidence handling. No store, no I/O.
- API serialization (unit tests in `api.rs`): new Request/Response variants round-trip as tagged JSON; old variants' JSON unchanged.
- Handler (integration tests against an in-memory/sqlite store and a fake `Env`): `handle(Request::Events{..})` etc. with seeded events, including job-ref resolution errors.
- CLI end to end (tests in `crates/pizza-delivery/tests` using the existing harness): spawn the daemon, seed ledger via harness runs or direct DB rows, run `inbox`/`events`/`artifacts` with flags, assert on `--json` output and exit codes.
- Not covered: UI rendering (no UI exists), SQL performance on large ledgers.
