---
id: ingest-handoff-feedback
blocked_by: [gate,handoff-items,feedback-model,gate-config]
---

# Ingest handoff items through the gate

In `worker.rs::accept_handoff`, after validation, parse items, gate each with the configured thresholds, append `FeedbackRecorded` for Inbox and `FeedbackSuppressed` for the rest. Applies to all stages. Budget-cap, unresolved-repo and diff-policy feedback keep bypassing the gate. Ingestion failures must not fail the stage. Integration tests with scripted handoffs: mixed items yield 1 recorded + 2 suppressed; item without metadata reaches inbox.
