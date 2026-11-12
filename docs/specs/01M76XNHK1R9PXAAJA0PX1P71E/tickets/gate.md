---
id: gate
blocked_by: []
---

# Pure confidence gate

Add `crates/pizza-delivery-core/src/gate.rs` with `Impact` (low, medium, high, irreversible), `GateThresholds` (defaults 50/80), `GateItem { confidence: Option<u8>, impact: Option<Impact> }` and `gate() -> GateDecision { Inbox, HandoffOnly, AutoResolved }`. Unknown confidence or impact fails safe to Inbox. Export from lib.rs. Unit tests: full default table, boundaries 49/50/79/80, custom thresholds, unknowns.
