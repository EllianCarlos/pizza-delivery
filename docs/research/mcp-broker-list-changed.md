# Dynamic MCP broker: list_changed end to end

Question from the spec: does a broker MCP that emits `notifications/tools/list_changed` work end to end, and does the claude streaming SDK (`setMcpServers`) matter?

## Design chosen

- `pizza-delivery mcp-broker` is a stdio MCP server started by the agent session through `--mcp-config` (claude adapter only for now). The worker writes `mcp.json` into the stage directory and passes it as `--mcp-config <path>`. It is only written when a catalog exists at `PIZZA_DELIVERY_MCP_CATALOG` (default `<data dir>/mcp-catalog.toml`).
- It advertises `capabilities.tools.listChanged = true`, always exposes `mcp_list`, `mcp_activate`, `mcp_deactivate` and `mcp_call`, and after a successful activation, deactivation or expiry it sends `notifications/tools/list_changed`. Activated tools are listed as `<server>__<tool>` and are also callable directly.
- No `setMcpServers` or streaming SDK input is needed: the single broker server is declared once at launch, and the dynamic part is its own tool list. `mcp_call` is the fallback for clients that ignore `list_changed`.
- Credentials are read by name from a daemon-side directory (`PIZZA_DELIVERY_SECRETS`, default `<data dir>/secrets`, one file per credential) and set only in the environment of the spawned server process.
- Servers live in the broker's process group, which is the agent's group. `CliAgent` now kills the whole group after the agent exits, so a crashed or abandoned session cannot leave servers behind. Closing the broker's stdin also ends all servers.
- Events are written by the broker to `mcp-events.jsonl` in the stage directory and ledgered by the worker after the run (`mcp-requested`, `mcp-activated`, `mcp-denied`, `mcp-calls`, `mcp-deactivated`). Denials also become `feedback-recorded` entries. Activated server names fill `mcp_activations` in the sandbox audit.

## What was verified

Verified with a scripted MCP client against the real broker binary (`crates/pizza-delivery/tests/mcp_broker.rs`) and a fake stdio server:

- `initialize` reports `listChanged: true`.
- After `mcp_activate`, the broker emits `notifications/tools/list_changed`, and a following `tools/list` contains `docs__echo`. After `mcp_deactivate` and after an idle timeout the same notification arrives and the tool disappears.
- Direct namespaced calls and `mcp_call` both reach the server; per-server call counts are ledgered.
- Credentials appear in the server process environment and nowhere in broker output or events.

Through the full daemon (`crates/pizza-delivery/tests/mcp_stage.rs`), with a fake agent driving the broker named in `mcp.json`: the ledger gets the MCP events and a feedback entry for a denial, and servers are dead after an agent that exits without cleanup.

## Not verified

A live `claude -p` session consuming the broker was not run, because the automated attempt was blocked by the permission classifier. That session is the remaining open check:

```
claude -p "<activate docs, then call docs__echo>" --mcp-config mcp.json --strict-mcp-config --output-format stream-json --verbose
```

What to look for in the stream: whether `mcp__pizza-delivery__docs__echo` appears in a later turn's tool list after activation (client honours `list_changed`), and otherwise that `mcp__pizza-delivery__mcp_call` works. Until confirmed, prompts must tell agents to use `mcp_call` when a namespaced tool does not show up. `opencode` and `agy` pickup of in-flight tool lists, and `--mcp-config` support for them, remain open; they get no broker today.

## Known gaps

- Catalog entries with only a `url` are accepted but fail at activation ("url servers are not supported by the stdio broker yet") and are ledgered as deactivated with reason `launch-failed`.
- Egress hosts of an activated server are ledgered on `mcp-activated` but do not yet extend a running sandbox's allowlist.
