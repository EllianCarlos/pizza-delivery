---
id: tui-form
blocked_by: []
---

# TUI new-job form with collapsed Additional Parameters

Turn the `n` new-job mode of the core `tui::Model` into a form (`NewJobForm`): prompt, collapsed "> Additional Parameters" row, and the five options when open. `Command::Submit` carries `options` (none when closed or empty). Tests: focus order and typing in the reducer, `options` mapping, `TestBackend` frames for the closed and open states.
