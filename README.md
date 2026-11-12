# pizza-delivery

Autonomous prompt queue: `pizza-delivery "<prompt>"` enqueues a job that local AI agents triage, spec, implement and review, ending at a draft PR.

Submit forms with a collapsed "> Additional Parameters" group (template, report only, max stages, context hint, risk flags):

- CLI: `pizza-delivery --help` lists the flags under "Additional Parameters".
- TUI: `pizza-delivery tui`, then `n` for a new job (Tab moves, Enter on the expander toggles it, Enter submits, Esc cancels).
- Web: open the web UI (default <http://127.0.0.1:7878/>), choose "Submit a job" and expand "Additional Parameters".

## Deploying

`scripts/deploy.sh` rolls the user service forward to `origin/main`: it syncs the deploy checkout (default `~/Projects/pizza-delivery-deploy`), builds `web/` with bun and the release binary with cargo (honouring a redirected cargo target dir), installs the binary atomically at the path in the unit's `ExecStart`, restarts `pizza-delivery-daemon.service`, and verifies that the new pid listens on `127.0.0.1:7878`, the unit is active and `/app.js` is non-empty. Every step fails loudly with its own message.

```sh
scripts/deploy.sh                    # build, install, restart, verify
scripts/deploy.sh --no-restart       # build and install only
scripts/deploy.sh --deploy-dir DIR --unit UNIT --port PORT
```

It expects `git`, `bun`, `cargo`, `curl`, `systemctl` and `ss` on PATH (for example `devenv shell -- scripts/deploy.sh`). The deploy checkout is hard-reset to `origin/main`; keep no local work in it.

Status: design phase. Start with [docs/designs/pizza-delivery.md](docs/designs/pizza-delivery.md) and [GLOSSARY.md](GLOSSARY.md).

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.
