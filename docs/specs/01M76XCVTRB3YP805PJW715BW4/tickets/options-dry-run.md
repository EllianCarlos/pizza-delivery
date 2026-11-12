---
id: options-dry-run
blocked_by: [options-wire]
---

# Apply options in triage and dry-run

Pass the submitted options into `conclude` in `crates/pizza-delivery/src/triage.rs` and `server.rs` so `Request::Triage` and the real triage stage use them. Tests in `crates/pizza-delivery/tests/triage.rs`: dry-run reflects options and enqueues nothing; a submitted job's plan honours report-only and max-stages.
