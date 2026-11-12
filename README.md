# pizza-delivery

Autonomous prompt queue: `pizza-delivery "<prompt>"` enqueues a job that local AI agents triage, spec, implement and review, ending at a draft PR.

Submit forms with a collapsed "> Additional Parameters" group (template, report only, max stages, context hint, risk flags):

- CLI: `pizza-delivery --help` lists the flags under "Additional Parameters".
- TUI: `pizza-delivery tui` (Tab moves, Enter toggles, Ctrl-P previews triage, Ctrl-S submits, Esc quits).
- Web: set `PIZZA_DELIVERY_HTTP_LISTEN=127.0.0.1:7878`, restart the daemon and open <http://127.0.0.1:7878/>. Loopback only and unauthenticated, so any local process can submit while it is on.

Status: design phase. Start with [docs/designs/pizza-delivery.md](docs/designs/pizza-delivery.md) and [GLOSSARY.md](GLOSSARY.md).

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.
