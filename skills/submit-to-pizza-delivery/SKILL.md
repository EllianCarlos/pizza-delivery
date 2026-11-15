---
name: submit-to-pizza-delivery
description: Use when work should be queued into the pizza-delivery app instead of done inline — the user says "submit/queue/send this to pizza delivery", or a task is large, independent and self-contained enough to hand to the autonomous queue. Covers writing the prompt, choosing flags, submitting, and following up.
---

# Submitting to pizza-delivery

`pizza-delivery "<prompt>"` enqueues a job. Local agents triage, disambiguate, spec, implement and review it, then ship a draft PR (auto-merged when CI is green and the diff policy allows). Agents never ask the human questions: assumptions, questions and risks land in the inbox.

## When to submit

Submit: self-contained work with a clear outcome, ideally one repo, that does not need this session's context.

Do not submit: work the user wants to steer interactively, or a one-line change that is faster inline. If unsure, ask once.

## Write the prompt

The queue's agents see only the prompt, memory (engram, mnemon) and the repo. So:

- State the outcome and acceptance criteria, not steps.
- Name the repo path, PR URL, files or issue numbers involved.
- Say what NOT to do ("do not change anything, only report").
- Never paste secrets. Never reference "this conversation".

## Submit

```bash
pizza-delivery --repo /abs/path/to/repo "<prompt>"
```

- **Always pass `--repo`.** Without it triage guesses from the prompt and can pick the wrong or an empty repo. Use the absolute path of the checkout.
- `--dry-run` prints the plan and context sources without enqueuing. Use it when unsure of the plan.
- Output is a JSON line with the job `id` and `short_id`. Report the short id to the user.

Optional flags (all unset lets triage decide):

| Flag | Use for |
|---|---|
| `--template full-feature\|bug-fix\|investigate` | force the plan shape |
| `--report-only` | findings only: no implement, review or ship |
| `--max-stages N` | cap stages (1 to 10) |
| `--context "<text>"` | extra hint appended to the task |
| `--risk <flag>` | assert a risk (kebab-case, repeatable) |
| `--model <model>` | Claude model for every stage (e.g. haiku, sonnet) |
| `--priority low\|normal\|high` | queue order |

Questions, "should we X", investigations: use `--report-only` or `--template investigate`.

## Follow up

```bash
pizza-delivery status                # list jobs
pizza-delivery status @last          # latest job's timeline
pizza-delivery status <id-prefix>    # one job
pizza-delivery inbox                 # assumptions, questions, risks
pizza-delivery reply <job> -m "<answer>"   # answer feedback; spawns one follow-up job
pizza-delivery cancel <job>
```

A failed job states why in its `job-failed` event. Resubmit with a corrected prompt rather than retrying blindly. Do not poll in a loop: submit, report the id, and check once when asked.

## Do not

- Do not submit the same task twice; check `status` first.
- Do not edit the job's worktree under `~/.local/share/pizza-delivery/worktrees`.
- Do not restart the daemon while jobs run: it kills running agents (they are requeued).
