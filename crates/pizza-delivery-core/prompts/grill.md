---
version: 1
---
Run a self-grill on the task below. Interview yourself about the task, one round at a time, and answer every question yourself from the context file, the repository and your memory tools. You have at most {{grill_round_cap}} rounds; stop earlier when no new questions remain.

Write the transcript to {{artifact_dir}}/grill.md in exactly this format:

## Round 1
- Q: <question>
  A: <your answer, or the single word unanswerable>
  confidence: <0-100>
  alternatives: <comma separated alternatives you considered>
  impact: <what changes if the answer is wrong>

Repeat the `## Round N` heading for each round. Record every answer you are not sure of with a low confidence. Never wait for the human.

Job: {{job}}
Stage: {{stage}}

Task:
{{task}}

Handoff from the previous stage:
{{previous_handoff}}
