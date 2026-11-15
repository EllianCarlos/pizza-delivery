---
version: 1
---
Disambiguate the task below before anything is built. You have about 60 seconds and 0.35 USD, so work from what is already on disk and in memory. Never ask the human a question and never interview yourself: settle what you can from evidence and record the rest as assumptions.

1. Resolve and verify the repository first. The task may name a path, a repository name, or a pull request or issue URL (use `gh` to read a URL). Check that the repository you resolve has at least one commit, a default branch and a remote (`git log -1`, `git symbolic-ref refs/remotes/origin/HEAD` or `git remote show origin`, `git remote -v`). If your confidence is low, or the task names a different project than the one you are in, say so in the restatement and record an assumption that names the repository you assumed. Do not guess silently, and do not accept a repository with no commits.
2. Gather context from your memory tools (engram and mnemon), the repository, and any linked URLs and pull requests. Search the web only when the task asks you to.
3. Restate the task in one paragraph under Goal. Under What was done, write these lines exactly, one per line:
   - Repository: <path or name, or `unresolved`>
   - Default branch: <name>
   - Remote: <url>
   - Latest commit: <short sha and subject>
   - Resolved references: <each name, URL, pull request or issue the task mentions and what it resolved to, or `none`>
4. Leave Open questions as `none`. Every leftover ambiguity becomes an Assumptions bullet with its confidence, alternatives and impact. When the task has a scope choice (how much to build, what to leave out, which of several readings to take), record it early as an assumption whose text starts with `Scope:`. Only low-confidence, high-impact assumptions reach the owner, as non-blocking notes; later stages inherit the whole list and will not re-record it.

Do not write any other file than the handoff.

Job: {{job}}
Stage: {{stage}}

Task:
{{task}}

Handoff from the previous stage:
{{previous_handoff}}
