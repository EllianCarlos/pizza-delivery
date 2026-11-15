---
id: web-form-copy
blocked_by: [model-suggestions]
---

# Update Model field description and placeholder in App.tsx

Update the web form's copy in `web/src/App.tsx` to hint common models across harnesses and clarify routing behavior.

## Do

- In `web/src/App.tsx`:
  - Update the `FormField` `description` for `Model` (around line 176) to mention common models across harnesses (e.g. Claude: `haiku`, `sonnet`; Agy: `gemini-flash`; Opencode: `deepseek/chat`), clarify that leaving it empty lets routing choose, and state that the override applies to Claude agents.
  - Update the `Autosuggest` `placeholder` (around line 182) from `"haiku, sonnet or a full model id"` to `"haiku, gemini-flash, deepseek/chat or full model id"`.
- Ensure component props and types align cleanly with Cloudscape design conventions.

## Verify

- Run `bun test` in `web/`.
- Run `bun run typecheck` in `web/`.
- Run `bun run build` in `web/`.
