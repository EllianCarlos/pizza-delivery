---
version: 2
---
Write a specification for the task below, then break it into tickets. Both are local Markdown files committed in the job worktree (your working directory); do not use an external tracker.

Write the spec to {{spec_dir}}/spec.md with these `##` sections, none of them empty: Problem, Solution, Acceptance criteria, Seams. State the seams that tests will cover; when unsure about a seam, pick the most likely one and list it under Assumptions in the handoff.

Write one file per ticket to {{spec_dir}}/tickets/<id>.md. Each starts with frontmatter:

---
id: <unique ticket id>
blocked_by: [<ids of tickets that must finish first>]
---

The ticket graph must be acyclic and every blocker must exist. Commit the spec and the tickets to the branch.

Job: {{job}}
Stage: {{stage}}

Task:
{{task}}

Handoff from the previous stage:
{{previous_handoff}}
