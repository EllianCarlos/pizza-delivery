# Multi-harness model hints and suggestions in the web submit form

## Problem

In the pizza delivery web UI (`web/src/App.tsx`), the "Additional Parameters" section in the new job modal exposes a `Model` form field. The current description is:
`"Claude model for every stage, e.g. haiku or sonnet. Empty lets routing choose."`
and the suggestion list (`MODEL_SUGGESTIONS` in `web/src/options.ts`) contains only Anthropic Claude models: `haiku`, `sonnet`, and `opus`. The placeholder text in the `Autosuggest` input is `"haiku, sonnet or a full model id"`.

Pizza delivery supports multiple agent harnesses beyond Claude Code: `agy`, `opencode`, and `hermes` (as declared in `AdapterKind` in `crates/pizza-delivery-core/src/routing.rs` and executed via `crates/pizza-delivery/src/agent.rs`). Users submitting jobs for multi-agent workflows find the Claude-centric copy misleading or exclusionary.

At the same time, `resolve_models` in `crates/pizza-delivery-core/src/routing.rs:421-423` only applies `job_model` to Claude candidates (`inputs.job_model.filter(|_| claude)`), falling back to rule chains, the `[[models]]` table, or harness defaults for other adapters. Changing routing semantics is out of scope and would risk breaking existing routing chains and CLI invocations. Therefore, the web UI description and suggestion list should hint common models for other harnesses (e.g. `gemini-flash` for Agy, `deepseek/chat` for Opencode) while clarifying that empty lets routing choose and that the job-level override applies to Claude agents.

## Solution

1. **Model Suggestions (`web/src/options.ts`)**:
   - Update `MODEL_SUGGESTIONS` to offer verified models across multiple harnesses already present in repository documentation and tests:
     - `haiku`: "Claude: cheapest; default for triage and memorize"
     - `sonnet`: "Claude: default for grill, spec, implement, fix and review"
     - `opus`: "Claude: hard or risky jobs"
     - `gemini-flash`: "Agy: fast Gemini model" (referenced in `crates/pizza-delivery-core/src/routing.rs:594`)
     - `deepseek/chat`: "Opencode: provider/model format" (referenced in `crates/pizza-delivery-core/src/routing.rs:595`)
   - Ensure all values remain unique and non-empty so that `buildOptions` accepts them cleanly.

2. **Web Form Copy (`web/src/App.tsx`)**:
   - Update the `FormField` description for Model at line 176 to:
     `"Model for every stage, e.g. haiku or sonnet (Claude), gemini-flash (agy), deepseek/chat (opencode). Empty lets routing choose; applies to Claude agents."`
   - Update the `Autosuggest` placeholder at line 182 to:
     `"haiku, gemini-flash, deepseek/chat or full model id"`

3. **Options Unit Tests (`web/src/options.test.ts`)**:
   - Update or extend existing tests in `web/src/options.test.ts` to assert that `MODEL_SUGGESTIONS` includes entries for `gemini-flash` and `deepseek/chat` alongside Claude models, with distinct values and harness-identifying descriptions.

4. **TUI, CLI, and Documentation Consistency**:
   - Verify `crates/pizza-delivery-core/src/tui.rs` (which renders only `"  Model"` with no description text, hence requiring no changes).
   - Verify `README.md:38` accurately reflects that `--model` / Model form field applies to Claude agents only while empty delegates to routing chains or harness defaults.

## Acceptance criteria

- `MODEL_SUGGESTIONS` in `web/src/options.ts` contains suggestions representing `claude` (`haiku`, `sonnet`, `opus`), `agy` (`gemini-flash`), and `opencode` (`deepseek/chat`).
- Every entry in `MODEL_SUGGESTIONS` has a unique, non-empty `value` and an informative `description` indicating its target harness.
- `web/src/App.tsx` renders the Model `FormField` with description mentioning models across Claude, Agy, and Opencode, and noting that empty lets routing choose and the field applies to Claude agents.
- The `Autosuggest` placeholder in `web/src/App.tsx` gives multi-harness examples.
- `web/src/options.test.ts` passes (`bun test`) and explicitly validates that suggestions contain entries for the non-Claude harnesses.
- TypeScript check (`bun run typecheck` in `web/`) and build (`bun run build` in `web/`) pass without errors or warnings.
- Backend routing (`crates/pizza-delivery-core/src/routing.rs`) and CLI behavior remain unchanged.

## Seams

- **`web/src/options.test.ts` (Automated test seam)**:
  Runs via `bun test` in the `web/` directory. Tests `MODEL_SUGGESTIONS` completeness, harness representation, uniqueness, and compatibility with `buildOptions`.
- **TypeScript and Build (`bun run typecheck`, `bun run build`) (Compilation seam)**:
  Ensures prop types for Cloudscape's `FormField` and `Autosuggest` in `web/src/App.tsx` match the component definitions and no type errors exist.
- **Manual Web Form Inspection (Visual / UI seam)**:
  Inspection of the "Additional Parameters" collapsible section in the job submission modal to ensure text wrapping and rendering look clean at desktop and mobile widths.
