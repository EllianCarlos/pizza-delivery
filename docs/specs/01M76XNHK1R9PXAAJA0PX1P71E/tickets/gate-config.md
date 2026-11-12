---
id: gate-config
blocked_by: []
---

# Gate thresholds in daemon config

Add `gate_inbox_below` and `gate_auto_at` to `Config` read from `PIZZA_DELIVERY_GATE_INBOX_BELOW` / `PIZZA_DELIVERY_GATE_AUTO_AT` (defaults 50/80). Reject non-numeric, >100, or inbox_below > auto_at with a ConfigError. Tests via `Config::from_lookup`.
