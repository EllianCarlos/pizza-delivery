---
id: options-core
blocked_by: []
---

# Triage options model and application

Add `TriageOptions` (template, report_only, max_stages, context_hint, risk_flags) with validation to `crates/pizza-delivery-core/src/triage.rs`. Thread it through `triage()` and `conclude()`; emit the low-severity `assumption` feedback when an override disagrees with the heuristic. Unit tests for every option, combinations, validation errors and the no-options identity. Export from `lib.rs`.
