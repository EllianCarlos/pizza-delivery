# Agent CLI fixtures and findings

Issue #4, parent spec #1. Captured 2026-11-09 on the owner's machine with tiny prompts. Fixtures live in `docs/fixtures/<cli>/`, one `<case>.stdout.*`, `<case>.stderr.*` and `<case>.exitcode` triple per case. Paths, hook output and user-specific text were scrubbed (see Scrubbing).

Versions: claude 2.1.316, Hermes Agent 0.21.5, opencode 2.2.1, agy 1.4.8.

## Summary of what could and could not be observed

| CLI | Normal run | Quota-exhausted run | Skill expansion |
| --- | --- | --- | --- |
| claude | captured | not observed (unknown) | works, including user-invoked-only skills |
| opencode | captured | not observed (unknown) | does not expand slash-prefixed text |
| Hermes | not possible (no credentials in this environment) | unknown | unknown |
| agy | not possible (no credentials in this environment) | unknown | unknown |

No quota exhaustion was provoked on any CLI. The only quota-adjacent data is claude's `rate_limit_event` while allowed. Everything the classifiers need for quota-exhausted shapes remains an explicit unknown for all four CLIs.

## claude

Invocation: `claude -p "<prompt>" --output-format stream-json --verbose`. `--verbose` is required for stream-json in print mode.

Stream shape (`claude/normal.stdout.jsonl`):

- `system/hook_started` and `system/hook_response` lines first, one pair per SessionStart hook. These come from the owner's hooks and plugins, so adapters must ignore unknown `system` subtypes.
- `system/init` with `session_id`, `model`, `tools`, `mcp_servers` (each with `status`, some `failed`), `permissionMode` and `slash_commands`.
- `assistant` messages with `message.content[]` blocks (`text`, `thinking`, tool use) and per-message `usage`.
- `rate_limit_event` after each assistant message: `rate_limit_info.status` (`allowed` observed), `rateLimitType` (`five_hour`), `resetsAt` (epoch seconds), `unifiedWindows.five_hour` and `unifiedWindows.seven_day` each with `utilization` (0 to 1), `resetsAt`, `status`, plus `overageStatus` and `overageDisabledReason`. This supports the proactive utilization threshold in the spec. Non-allowed `status` values are unobserved.
- Final `result` line: `subtype`, `is_error`, `result`, `terminal_reason`, `session_id`, `total_cost_usd`, `usage`, `modelUsage`, `duration_ms`, `num_turns`, `permission_denials`.

### Exit codes versus result JSON

| Case | Fixture | Exit | `subtype` | `is_error` | `terminal_reason` |
| --- | --- | --- | --- | --- | --- |
| Normal | `normal` | 0 | `success` | false | completed |
| `--max-turns 1` | `normal-maxturns1-stophook` | 1 | `error_max_turns` | true | `max_turns` |
| Unknown model | `error-bad-model` | 1 | `success` | true | `api_error` (`api_error_status` 404) |
| `--max-budget-usd 0.001` | `error-budget-cap` | 1 | `error_max_budget_usd` | true | `budget_exhausted` |

Findings:

- Confirms the spec rule that the exit code is never trusted alone: an unknown model reports `subtype: success` with `is_error: true`. Classify on `is_error`, `terminal_reason` and `api_error_status`, not on `subtype` or exit code.
- A Stop hook that returns feedback makes claude run an extra turn (a synthetic `user` message with `isSynthetic: true`, then another assistant message). With `--max-turns 1` this turned a good answer into `error_max_turns`. Adapters should not set a tight `--max-turns` when user hooks exist, and should read the answer from the last `assistant` text if the result is `error_max_turns`.
- Hook feedback also appends the model's extra reply (`nts` here) after the real answer, and `result.result` is that last reply: in `normal` the prompt asked for `pong` but `result` is `nts`. Neither `result.result` nor the last assistant text is reliably the stage output when Stop hooks are active. Adapters should run claude with the owner's hooks disabled or accumulate all assistant text, and the daemon should decide this before building stage output extraction.
- Bad model also prints `[claude-code:unrecognized_model] {...}` on stderr.
- The budget cap is checked after a turn, so it overshoots (0.14 USD spent against a 0.001 cap).
- Quota-exhausted shape: unknown. Not provoked.

### Skill expansion

Fixtures: `skill-to-spec*`, `skill-implement-spec*`, `skill-unknown`.

- `/to-spec <args>` and `/implement-spec <args>` expand headlessly under `-p`, even though both skills have `disable-model-invocation: true`. Proof: asked to quote the first sentence of the expanded instructions, claude returned "This skill takes the current conversation context and codebase understanding and produces a spec." and "You have been provided a spec." (`*-probe` fixtures). Both exit 0.
- Expansion is not visible as a distinct stream event. `input_transformations` on assistant messages was empty, and no `user` message carrying the skill body appears in the stream. Verification must come from `system/init.slash_commands`, which lists `to-spec` and `implement-spec` when installed.
- Unknown skill: `/nonexistent-skill-xyz hello` exits 0 with `subtype: success`. The model explains the skill is not installed and treats the rest as plain text. No error signal exists in the stream, so the pre-flight check against `init.slash_commands` (or an on-disk check) is the only way to fail fast on a configuration error. The init line comes before any model call, so the adapter can kill the process as soon as it sees a missing skill.
- Slash-command listing is large (about 200 entries on this machine) and includes plugin namespaced names such as `plugin:skill`.

## opencode

Invocation: `opencode run --format json "<prompt>"`. Output is JSONL, one event per line, each with `type`, `timestamp`, `sessionID` and a `part` object.

Stream shape (`opencode/normal.stdout.jsonl`): `step_start`, `tool_use` (with `part.tool`, `part.state.status`, `input`, `output`), `step_finish` (with `reason`, `cost`, `tokens.{input,output,reasoning,cache}`), `text` (`part.text`), final `step_finish` with `reason: stop`. There is no dedicated result event; the end of the stream with a `step_finish` whose `reason` is `stop` marks completion. Normal exit code 0. The final text is `pong`.

Observations:

- The model here auto-called a `skill` tool (`using-superpowers`) before answering, so a stage's tool calls can appear even for trivial prompts.
- Error shape (`error-bad-model`): `-m nosuch/model-xyz` exits 1 with a single stdout line `{"type":"error","timestamp":...,"sessionID":...,"error":{"type":"provider.no-route","message":"Model unavailable: nosuch/model-xyz"}}` and nothing on stderr. Errors arrive on stdout as JSON, with the exit code 1. This is the best available guess for how provider errors look, but quota errors specifically are unobserved.
- Skill expansion: not supported via slash text. `/to-spec ...` and `/implement-spec ...` were passed as plain prompts; the model reported it received no skill text (`NO_SKILL_TEXT`). `/nonexistent-skill-xyz hello` also exits 0, and the model attempted its own `skill` tool and said the skill was not found. opencode can use skills only through its model-driven `skill` tool, which means skill-driven stages cannot be routed to opencode, matching the capability rule in the spec.
- Quota-exhausted shape: unknown.

## Hermes

Invocation: `hermes chat -q "<prompt>" -Q --format stream-json --max-turns 1`.

Only the credential failure could be captured (`hermes/normal.*`). Hermes defaults to a `gemini-3.6-flash` model and no `GOOGLE_API_KEY` or `GEMINI_API_KEY` is available to this agent environment, so no normal run exists.

- stdout is two JSONL lines: `{"type":"system","subtype":"init","model":...,"session_id":...,"timestamp":...}` and `{"type":"result","session_id":...,"exit_code":1,"text":"","tokens":{...},"duration_ms":...,"error":"credentials or agent init failed",...}`.
- stderr: `No usable credentials found for provider 'gemini'. Set GOOGLE_API_KEY, GEMINI_API_KEY.` followed by `session_id: <id>`.
- Exit code 1. The result line carries `exit_code` and a short `error`, so a classifier can distinguish a configuration error from other failures, but the human text is on stderr.
- Hermes lists `to-spec` and `implement-spec` as enabled local skills (`hermes skills list`) and offers `-s/--skills` to preload skills, but expansion could not be tested without a working model.
- Unknown: normal event stream (assistant, tool, usage events), quota-exhausted shape, skill expansion behaviour, how a `-s` preload of a missing skill fails.

## agy

Invocation: `agy -p "<prompt>" --output-format json`. Supports `--disable-slash-commands` ("Disable slash command and skill expansion in print mode"), which implies slash expansion exists in print mode.

Only the credential failure could be captured (`agy/normal.*`): exit code 1, empty stdout, stderr `modelProvider is set to "gemini" in settings.json, but the GEMINI_API_KEY environment variable is not set. ...`. In this environment the key is not set, and credential files were deliberately not inspected.

- Unknown: normal JSON output shape, `stream-json` shape, quota-exhausted shape and exit code, skill expansion behaviour for `to-spec` and `implement-spec`.
- Note: output on failure is stderr text only, so an agy classifier must match text, not JSON.

## Consequences for adapters and classifiers

- claude: parse `result` for `is_error`, `terminal_reason`, `api_error_status`; read `rate_limit_event` for proactive quota; preflight skills against `system/init.slash_commands`; tolerate hook noise (unknown `system` subtypes, synthetic `user` messages, trailing extra assistant turns).
- opencode: no result event, no skill expansion; detect failure via exit code plus a stdout `type: error` event; unknown quota shape means the reactive classifier must start as "any non-zero exit with an error event" until a real quota error is captured.
- Hermes and agy: only configuration failures are known. Treat them as terminal configuration errors, and keep the quota classifier disabled until a real fixture exists.
- The spec's "Known CLI facts" line is confirmed for claude and opencode flag names. Hermes `--format stream-json` and `agy -p --output-format json` are accepted as flags (no usage error) but their success output is unverified.

## Follow-up needed to close the unknowns

1. Provide `GEMINI_API_KEY` (or the provider configured for Hermes and agy) to a one-off capture run, then record normal runs and skill expansion for both.
2. Capture a real quota exhaustion for each CLI opportunistically, from the daemon's own raw-output artifacts once running, rather than provoking one.

## Scrubbing

Hook output and synthetic hook-feedback text were replaced with `[redacted ...]`, thinking signatures were redacted, the home directory and username were replaced with `/home/user` and `user`, and the scratch cwd was replaced with `/tmp/scratch/run`. Session ids and message ids are left as is since they are not sensitive. MCP server names in `system/init` are kept. A grep for the username, email and common key prefixes found only MCP server names.

## Conservative quota classifiers (issue #7)

Implemented in `crates/pizza-delivery-core/src/classify.rs`. Every quota shape below is an assumption, not an observation, until a real exhaustion is captured; all recorded fixtures classify as success, provider error or configuration error, never as quota.

- claude: a `rate_limit_event` whose `status` does not start with `allowed` is treated as exhausted, with the reset taken from the hottest `unifiedWindows` entry (else `resetsAt`). An `is_error` result is quota when `api_error_status` is 429, a non-allowed rate-limit event was seen, or the result text or reason mentions a rate or usage limit. 5xx and 529 are transient. Anything else, including the budget cap and unknown model, is a terminal provider error.
- opencode and agy (reactive): a non-zero exit whose stdout or stderr mentions rate limit, usage limit, quota, too many requests, resource exhausted or a standalone 429 is quota with an unknown reset (default backoff applies). Otherwise a non-zero exit with a `type: error` stdout event (opencode) or any non-zero exit (agy) is a terminal provider error, and a silent non-zero exit or signal death is a crash.
- Hermes: quota classification stays disabled; credential failures are configuration errors, everything else non-zero is a provider error. agy credential failures (`environment variable is not set`) are configuration errors too.
- Skill pre-flight: when a stage names a required skill and `system/init.slash_commands` is non-empty but lacks it, the failure is a configuration error. No stage passes a skill name yet.
