---
id: hard-stops-regression
blocked_by: [ingest-handoff-feedback,prompt-preamble]
---

# Hard-stop and behavior regression tests

Add integration tests proving budget-cap overrun, unresolved repo and diff-policy risk still surface as `feedback-recorded` regardless of thresholds, that budget-cap parking and reply-spawns-one-follow-up are unchanged, and that `agent-invoked` records template version 2. Update GLOSSARY.md (confidence gate, suppressed feedback) and add a short ADR in docs/adr/.
