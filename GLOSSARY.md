# Pizza Delivery glossary

**Job**: one submitted prompt and its whole run through the pipeline.

**Stage**: one pipeline step inside a job (for example self-grill, spec, implement, review).

**Handoff**: the validated document a finishing stage passes to the next stage.

**Feedback entry**: something the agent surfaces for the human: an assumption it made, a question it could not answer, or a risk it noticed.

**Confidence gate**: the pure rule that decides whether an assumption, open question or risk from a handoff reaches the inbox. Items below the inbox threshold (default 50) with high or irreversible impact go to the inbox; items at or above the auto threshold (default 80) are auto-resolved; the rest stay in the handoff. Missing or malformed confidence or impact metadata goes to the inbox. Budget-cap, unresolved-repo and diff-policy stops bypass the gate.

**Suppressed feedback**: a handoff item the confidence gate kept out of the inbox. It is recorded as a `feedback-suppressed` ledger event so nothing is dropped silently.

**Follow-up job**: a job created by the human replying to one or more feedback entries of an earlier job.

**Ledger**: the append-only log of everything that happened to every job.

**View**: one way of presenting the same set of jobs.

**Table**: the view that lists jobs as rows.

**Board**: the view that groups jobs into one column per job state.

**Pipeline**: the view that shows one lane per job with one segment per stage.

**Filter**: a set of property constraints that narrows a list of events, artifacts or feedback entries (the inbox). The inbox filter also covers an entry's status: open, answered or dismissed. Different properties are ANDed; repeated values of one property are ORed.

**Search**: a case-insensitive substring match over an item's text fields, ANDed with the filter.

**Triage options**: the optional "Additional Parameters" a submitter can give with a job (template, report-only, max stages, context hint, extra risk flags) to override or sharpen what triage would decide from the prompt alone.
