# pizza-delivery

Autonomous prompt queue: `pizza-delivery "<prompt>"` enqueues a job that local AI agents triage, spec, implement and review, ending at a pull request.

## Auto-merge

After the review stage passes, the daemon marks the job's PR ready and merges it with `gh pr merge --merge` (a merge commit, never a rebase-merge). It merges only when all of these hold; otherwise the PR stays open and a `pr-not-merged` ledger event records the reason (shown in the job detail's event list):

- the plan had a review stage and its final report has no blocking findings;
- the diff does not trip the diff policy (CI workflows, git hooks, `.envrc`, devenv or flake files, install scripts); those PRs stay labelled drafts;
- CI checks pass and the PR's mergeable state is clean (pending checks are awaited for up to 10 minutes);
- for a follow-up job, the parent job's PR is already merged (stacked PRs merge dependencies first).

Set `PIZZA_DELIVERY_AUTO_MERGE=off` (also `0`, `false`, `no`) to disable it; it defaults to on. A merged PR is recorded as a `pr-merged` event.

Submit forms with a collapsed "> Additional Parameters" group (template, report only, max stages, context hint, risk flags):

- CLI: `pizza-delivery --help` lists the flags under "Additional Parameters".
- TUI: `pizza-delivery tui`, then `n` for a new job (Tab moves, Enter on the expander toggles it, Enter submits, Esc cancels).
- Web: open the web UI (default <http://127.0.0.1:7878/>), choose "Submit a job" and expand "Additional Parameters".

Status: design phase. Start with [docs/designs/pizza-delivery.md](docs/designs/pizza-delivery.md) and [GLOSSARY.md](GLOSSARY.md).

Licensed under either of MIT ([LICENSE-MIT](LICENSE-MIT)) or Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)) at your option.
