---
id: options-cli
blocked_by: [options-wire]
---

# CLI flags under Additional Parameters

In `crates/pizza-delivery/src/main.rs` add `--template`, `--report-only`, `--max-stages`, `--context`, repeated `--risk`, all with `help_heading = "Additional Parameters"`. Build `options` only when at least one flag is set. Clap parse tests for mapping and for the no-flag case sending no options.
