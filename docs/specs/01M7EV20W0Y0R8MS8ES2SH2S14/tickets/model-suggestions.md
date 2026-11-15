---
id: model-suggestions
blocked_by: []
---

# Multi-harness model suggestions in options.ts and unit tests

Expand `MODEL_SUGGESTIONS` in `web/src/options.ts` to include verified models from other supported harnesses and cover them in `web/src/options.test.ts`.

## Do

- In `web/src/options.ts`, update `MODEL_SUGGESTIONS` with entries for `gemini-flash` (Agy) and `deepseek/chat` (Opencode) alongside the existing Claude entries (`haiku`, `sonnet`, `opus`).
- Prefix or annotate suggestion descriptions with harness labels (e.g. "Claude: ...", "Agy: ...", "Opencode: ...") for clarity.
- Keep all values unique and non-empty.
- In `web/src/options.test.ts`, extend the test suite to assert that `MODEL_SUGGESTIONS` includes entries for `gemini-flash` and `deepseek/chat`, and that all suggestions continue to be accepted by `buildOptions`.

## Verify

- Run `bun test` in `web/` to verify all options tests pass.
- Run `bun run typecheck` in `web/` to ensure type consistency.
