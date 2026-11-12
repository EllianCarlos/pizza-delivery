---
id: prompt-preamble
blocked_by: [handoff-items]
---

# Prompt preamble update

Extend the preamble in `prompt.rs` to require, per assumption/question/risk bullet, `[confidence: 0-100; alternatives: ...; impact: low|medium|high|irreversible; evidence: <source>]`, and to take the recommended answer when confidence is at or above the auto threshold (render threshold from config as a parameter). Bump `TEMPLATE_VERSION` to 2. Update prompt tests and any test asserting version 1.
