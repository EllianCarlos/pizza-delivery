---
version: 1
---
Review the changes on the current branch against the task, the spec under {{spec_dir}} if one exists, and the repository conventions. Do not change code in this stage.

Write the review report to {{artifact_dir}}/review.md with a `## Findings` section. List each finding as a bullet that starts with [blocking] (the change must not ship as is) or [non-blocking]. Write `None` when there are no findings. Add a `## Summary` section.

Job: {{job}}
Stage: {{stage}}

Task:
{{task}}

Handoff from the previous stage:
{{previous_handoff}}
