# Calibrated auto-grilling: only important questions reach the inbox

## Problem

The grill stage runs unattended, but every assumption and open question an agent writes into its handoff is a candidate for the human's attention, and nothing decides which ones matter. Today `handoff::validate` only checks that the `Assumptions` and `Open questions` sections exist; their bullets are never parsed or turned into feedback entries (the inbox, GLOSSARY "Feedback entry"). `Feedback` / `EventPayload::FeedbackRecorded` carry `kind`, `severity`, `text`, `confidence` but no `alternatives` or `impact`, although the design calls for them. Agents are told only to "take the recommended answer", with no guidance on how sure they must be, so they either over-ask or silently guess. The result is no independence signal for the agent and no filter for the human.

## Solution

1. **Pure gate** (`pizza_delivery_core::gate`): `gate(item, threshold) -> GateDecision` maps `(confidence, impact, reversibility)` to `Inbox`, `HandoffOnly` or `AutoResolved`. Default rule: confidence < 50 and impact `high` or `irreversible` → `Inbox`; confidence 50–79 (or < 50 with low/medium impact) → `HandoffOnly`; 80+ → `AutoResolved`. Impact scale: `low | medium | high | irreversible`. Thresholds come from a `GateThresholds { inbox_below: u8, auto_at: u8 }` value (defaults 50 / 80).
2. **Handoff item parser** (`handoff::items`): parses bullets of `Assumptions`, `Open questions` and `Risks` ending in `[confidence: N; alternatives: a, b; impact: X]`, plus an optional `evidence:` field citing the source. Malformed or missing brackets parse as `confidence: None`, which the gate treats as low confidence / unknown impact → `Inbox` (fail safe, never silently dropped). `validate` keeps its current behavior.
3. **Feedback model**: add optional `alternatives: Vec<String>` and `impact: Option<Impact>` (and `evidence`) to `Feedback` and `FeedbackRecorded`, with `#[serde(default, skip_serializing_if ...)]` so old ledger rows still deserialize. Feedback lives only in ledger events today (no feedback table, no ts-rs in the repo), so no SQL migration is needed; if ts-rs types exist when this lands, regenerate them.
4. **Suppressed items are ledgered**: new event `FeedbackSuppressed { kind, text, confidence, impact, decision }` for `HandoffOnly` and `AutoResolved` items, so nothing is dropped silently. `Inbox` items become `FeedbackRecorded`.
5. **Ingestion**: after a stage handoff is accepted (`worker.rs::accept_handoff`), parse items, gate each, and append events. Applies to every stage that writes assumptions, not only grill.
6. **Hard stops bypass the gate**: budget-cap overrun (`budget.rs`), unresolved repo (`triage.rs`) and diff-policy risks always produce `FeedbackRecorded`. Budget-cap pause and the reply-spawns-one-follow-up rule are untouched.
7. **Prompt**: `prompt.rs` preamble asks for per-item `[confidence; alternatives; impact]` with an evidence citation, and says to take the recommended answer when confidence is at or above the auto threshold; `TEMPLATE_VERSION` bumps 1 → 2.
8. **Config**: daemon config gains `PIZZA_DELIVERY_GATE_INBOX_BELOW` (default 50) and `PIZZA_DELIVERY_GATE_AUTO_AT` (default 80); invalid values (non-numeric, inbox_below > auto_at, > 100) are a `ConfigError`.

## Acceptance criteria

- `gate` is total and deterministic; unit tests cover every cell of the default table, the boundaries 49/50/79/80, custom thresholds, and unknown confidence/impact → `Inbox`.
- Parser extracts confidence, alternatives, impact and evidence from well-formed bullets in all three sections, tolerates `-`/`*` bullets and extra whitespace, and yields an item with unknown fields (not an error, not a drop) for malformed bullets.
- A handoff with one low-confidence high-impact item, one 60-confidence item and one 90-confidence item yields exactly one `feedback-recorded` and two `feedback-suppressed` ledger events.
- An item with no bracket metadata reaches the inbox.
- Budget-cap, unresolved-repo and diff-policy feedback still appear as `feedback-recorded` regardless of any confidence or threshold value; existing tests in `tests/budget.rs` and `tests/triage.rs` pass unchanged.
- Old `feedback-recorded` ledger rows without the new fields still deserialize.
- Rendered prompt contains the confidence/evidence instructions and records template version 2 in `agent-invoked`.
- Config rejects `inbox_below > auto_at` and values over 100; defaults are 50 and 80.
- Reply → follow-up job behavior and budget-cap parking are unchanged (existing tests pass).

## Seams

- Pure core functions, unit-tested: `gate::gate`, `handoff::items` parser, prompt rendering, config parsing via `Config::from_lookup` with a fake lookup.
- Ledger boundary: integration tests in `crates/pizza-delivery/tests/` using the existing harness with scripted agent handoffs, asserting on ledger event kinds and payloads (as `tests/triage.rs` and `tests/budget.rs` do).
- Worker ingestion goes through the existing `Store` trait; tests use the sqlite store via the harness, no mocks of the gate.
- The agent process remains the scripted fake; no real model calls.
