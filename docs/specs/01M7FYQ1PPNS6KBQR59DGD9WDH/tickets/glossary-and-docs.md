---
id: glossary-and-docs
blocked_by: [jobs-filter-ui]
---

# Update GLOSSARY.md with Finished jobs and Job state filter

Document the new terminology and behavior in `GLOSSARY.md`.

## Do

- In `GLOSSARY.md`, add definitions for:
  - **Finished jobs**: jobs in a terminal state (`completed`, `failed`, `cancelled`). In the Jobs page, they are hidden by default unless the "Show finished jobs" selector is enabled or an explicit state filter selects them.
  - **Job state filter**: a multi-select control in the Jobs page header that narrows the visible jobs to the selected states. When empty, visibility follows the "Show finished jobs" selector.

## Verify

- Inspect `GLOSSARY.md` to ensure terminology adheres to existing format and conventions.

## Done when

`GLOSSARY.md` is updated with definitions for finished jobs and job state filter.
