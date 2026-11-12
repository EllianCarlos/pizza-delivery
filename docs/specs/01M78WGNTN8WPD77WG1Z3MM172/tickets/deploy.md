---
id: deploy
blocked_by: [top-nav-ui]
---

# Deploy the merged header to the running daemon

Runs only after the `top-nav-ui` PR's merge commit is on `origin/main` (`git fetch origin && git merge-base --is-ancestor <merge sha> origin/main`). This must be the job's very last action.

## Do

1. Optionally list jobs (web API or `pizza-delivery` CLI) and prefer a moment with no other running stages; do not block indefinitely.
2. Launch the deploy outside the daemon's cgroup. The user manager's PATH has `bun` but not `cargo`, so `deploy.sh` must run inside the flake dev shell. Run it from a `/tmp` copy, because `deploy.sh` resets the deploy checkout (and its own file) to `origin/main` while bash is still reading it:
   ```sh
   ts=$(date +%s); d=$HOME/Projects/pizza-delivery-deploy
   cp "$d/scripts/deploy.sh" /tmp/pizza-deploy-$ts.sh
   systemd-run --user --unit=pizza-deploy-$ts --collect --working-directory="$d" \
     nix develop "$d" -c bash /tmp/pizza-deploy-$ts.sh
   ```
   Never run `scripts/deploy.sh` directly from the job (the agent lives in `pizza-delivery-daemon.service`'s cgroup and the restart would kill it). Never `systemctl stop` the daemon; `deploy.sh` uses `restart`. Do not add `git clean` (the deploy checkout has an untracked `.opencode/`).
3. Record the unit name in the handoff before launching, since the restart is expected to interrupt this job.

## Verify (by whoever resumes, or the next run)

- `journalctl --user -u pizza-deploy-<ts>` shows `deploy.sh` completing.
- `systemctl --user is-active pizza-delivery-daemon.service` → `active`.
- `git -C ~/Projects/pizza-delivery-deploy log -1` is the merge commit.
- https://pizza.elliancarlos.fyi shows the top bar.
