# Calibrated confidence gate for the inbox

Agents annotate each assumption, open question and risk with `[confidence; alternatives; impact; evidence]`. A pure function (`gate`) maps confidence and impact to inbox, handoff-only or auto-resolved, using thresholds from daemon config (default 50 and 80). Items kept out of the inbox are still ledgered as `feedback-suppressed`.

## Consequences

- Missing or malformed metadata fails safe to the inbox, so a careless agent is noisy rather than silent.
- Hard stops (budget cap, unresolved repo, diff policy) never pass through the gate.
- The gate trusts self-reported confidence; the `feedback-suppressed` trail is how miscalibration is audited.
- Feedback stays ledger-only; new fields are serde-defaulted so old rows still load.
- Ingestion is best effort and never fails a stage.
