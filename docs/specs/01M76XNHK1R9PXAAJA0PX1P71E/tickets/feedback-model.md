---
id: feedback-model
blocked_by: [gate]
---

# Feedback and event model

Add `alternatives`, `impact`, `evidence` (all serde-default, skipped when empty) to `triage::Feedback` and `EventPayload::FeedbackRecorded`; add `EventPayload::FeedbackSuppressed { kind, text, confidence, impact, decision }` with kind string `feedback-suppressed`. Update existing construction sites (budget.rs, triage.rs). Tests: round-trip serde, old rows without new fields deserialize, kind string. No SQL migration (feedback is ledger-only); regenerate ts-rs types only if present.
