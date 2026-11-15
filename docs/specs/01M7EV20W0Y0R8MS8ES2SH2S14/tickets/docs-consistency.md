---
id: docs-consistency
blocked_by: [web-form-copy]
---

# Verify TUI, CLI, and documentation consistency

Ensure related surfaces (TUI, CLI, documentation) are consistent with the updated web form model description and behavior.

## Do

- Inspect `crates/pizza-delivery-core/src/tui.rs` and verify that the TUI model form row does not contain conflicting or outdated description copy.
- Check `crates/pizza-delivery/src/main.rs` and verify CLI help text for the `--model` flag.
- Verify that `README.md` (around line 38) accurately describes model resolution order and confirms that the job-level model override applies to Claude agents while delegating other agents to routing chains.

## Verify

- Confirm no extraneous or unintended changes are introduced into core routing logic (`crates/pizza-delivery-core/src/routing.rs`).
- Run `bun test` in `web/` to ensure end-to-end web test suite continues to pass.
