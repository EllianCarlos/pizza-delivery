---
id: handoff-items
blocked_by: [gate]
---

# Handoff item parser

Add `handoff::items(text) -> Vec<HandoffItem>` for bullets in Assumptions, Open questions and Risks, parsing the trailing `[confidence: N; alternatives: a, b; impact: X; evidence: ...]`. Uses `gate::Impact`. Malformed or missing metadata yields unknown fields, never an error or a dropped bullet. `validate` unchanged. Unit tests for well-formed, bullet styles, whitespace, malformed, and empty sections ("none").
