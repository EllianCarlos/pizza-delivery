# Conciliate: keep concurrent pizza PRs mergeable without a human

## Problem

Several agent jobs open pull requests at the same time from `pizza/<jobid>` branches. They conflict with `main` and with each other, and a human had to resolve them by hand: merge `main` into each branch, then stack them (merge order #23, #24, #25, where each later PR already contains the earlier ones). GitHub's rebase-merge fails on stacked branches, so only merge commits work. Nothing in the pipeline does this, and two agents doing it at once would race on the same branches.

## Solution

A **conciliate** step that looks at the open `pizza/*` PRs of one repo, computes what is wrong, and either fixes it mechanically or reports it. It ships as a pure planner plus a CLI, `pizza-delivery conciliate [--repo <path>] [--dry-run]`.

1. **Discover**: `gh pr list --state open --json number,headRefName,headRefOid,baseRefName,url` in the repo, keeping branches that start with `pizza/` and whose base is the default branch. `git fetch origin` first; heads are read from `origin/<branch>`.
2. **Probe** (read-only, no worktree): `git merge-tree --write-tree --name-only <a> <b>`. Exit 0 is clean; exit 1 lists the conflicted paths. Probed for each PR against `origin/<default>`, and for each pair of PRs where neither contains the other. "Behind main" is `!git merge-base --is-ancestor origin/<default> <head>`. "Contains" is `git merge-base --is-ancestor <inner head> <outer head>`.
3. **Order into a stack**: a PR that already contains another PR's head goes after it (topological sort). Ties, and PRs unrelated by ancestry, order by PR number, which is the order a human would merge them.
4. **Plan**, one action per PR in stack order:
   - `up-to-date`: main is already an ancestor.
   - `merge-main`: behind main and the merge-tree probe is clean.
   - `conflict`: the probe reports conflicts with main, or with an earlier unrelated PR (reported on the later one). Never guessed at mechanically.
5. **Act** (without `--dry-run`, only the safe mechanical part): for each `merge-main` PR, in a throwaway detached worktree, `git merge origin/<default>` and a plain `git push origin HEAD:refs/heads/<branch>`. Never a force push; if the remote moved the push is rejected and reported, and the next run re-plans. Each outcome is a ledger event.
6. **Report conflicts, do not resolve them here**: each `conflict` becomes an inbox entry (a `feedback-recorded` risk on the job named by the branch, when that job exists in the ledger) and a ledger event. The entry carries the conflicted paths, the PR numbers involved and the default resolution side, so a human or the conflict-resolution stage can act on it.
7. **Conflict-resolution stage (delegated, later work)**: a stage that takes one inbox conflict entry, merges the other side into the PR branch in a worktree, resolves conflicts with the **stack side as the default** (for a PR against an earlier PR, the earlier stack member wins; for a PR against main, the branch side of files it owns wins and main's additions elsewhere are kept), runs the project's tests, and pushes with a plain push only if they pass. If the tests fail it pushes nothing and files a human-needed entry. This spec defines the contract; the CLI in this change does not run an agent.
8. **Locking** (best effort, so concurrent agents do not conciliate the same repo/PR at once): a lock file per key (`repo` or `repo#pr`) under `<data dir>/conciliate-locks/`, created with `O_EXCL`, holding `{ owner, pid, acquired_ms, heartbeat_ms, expires_ms }`.
   - A holder renews with a heartbeat that pushes `expires_ms` forward.
   - A lock is stale when `now > expires_ms` or its pid is no longer alive. A stale lock is taken over under a short-lived `.takeover` gate file (created `O_EXCL`; staleness is re-checked under the gate), so only one contender wins and a live lock is never removed.
   - Release deletes the file only if the unique token still matches, so a holder whose lock was taken over never removes the new holder's lock.
   - A refusal is a normal result, not an error: the CLI prints who holds the lock and until when, records nothing mutating, and exits 0. It never blocks or waits.
9. **Daemon trigger**: out of scope for this change (see Follow-ups).

## Acceptance criteria

- Planner is pure and deterministic; unit tests cover: independent PRs ordered by number, a PR containing another ordered after it regardless of number, behind-and-clean gives `merge-main`, up to date gives `up-to-date`, main conflict gives `conflict` and no merge action, pairwise conflict reported on the later PR only.
- Executor, using a fake git: `--dry-run` performs no merge, push, lock or ledger write; a normal run merges and pushes only `merge-main` PRs, records one ledger event per outcome, files one inbox entry per conflict, and does not file a duplicate on a second run with the same head.
- A failed merge or rejected push is reported and does not abort the other PRs.
- Lock tests: second acquire while held is refused with the holder's details; expired lock is taken over; dead-pid lock is taken over; heartbeat extends expiry; release removes the lock; a displaced holder's release leaves the new lock alone; concurrent contenders yield exactly one winner.
- A real-git test proves `merge-tree` conflict probing, the ancestor checks, and merge-main-then-plain-push against a local bare remote.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace` pass.

## Seams

- `pizza_delivery_core::conciliate`: pure `plan(PlanInput) -> Plan`, unit-tested with literal inputs.
- `Conciliator` trait (list PRs, ancestry, merge-tree probe, merge-and-push): `SystemConciliator` shells out to git and gh; tests use a fake. Kept as its own trait so the shared `Git` trait and `ship/git.rs` are untouched.
- `Recorder` trait over the ledger; a `Store`-backed implementation and a fake.
- Lock takes an injected clock and pid-liveness check.

## Follow-ups

- Daemon trigger: run conciliate for a job's repo after a stage opens a PR and on a slow timer, under the same lock.
- The conflict-resolution agent stage described in item 7.
- Web UI: add the new `conciliated` event kind to the kind filter list in `web/src/hooks.ts`.
