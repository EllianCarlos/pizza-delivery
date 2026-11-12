---
id: options-wire
blocked_by: [options-core]
---

# Carry options through API, event, job and store

Add `options: Option<TriageOptions>` to `Request::Submit`, `Request::Triage`, `EventPayload::JobSubmitted`, `NewJob`, `Job` (with `serde(default, skip_serializing_if)`), update `fold`, `handler.rs` (validate, usage error on bad values), and `sqlite_store.rs` persistence. Tests: serde back-compat in `api.rs`/`event.rs`, fold in `job.rs`, SQLite round trip, `tests/walking_skeleton.rs`. Update existing struct literals so current tests still pass.
