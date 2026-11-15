---
id: jobs-filter-ui
blocked_by: [filter-model]
---

# UI controls in JobsPage and empty states in views

Add the "Show finished jobs" toggle and state multi-select filter to `JobsPage` in `web/src/App.tsx`, wire filtered jobs to all views, and handle empty filter states.

## Do

- In `web/src/App.tsx`:
  - Import `Toggle` from `@cloudscape-design/components/toggle` and `Multiselect` from `@cloudscape-design/components/multiselect`.
  - In `JobsPage`:
    - Add state: `showFinished` (boolean, default `false`) and `selectedStates` (`JobState[]`, default `[]`).
    - Filter jobs using `visibleJobs(jobs, new Set(selectedStates), showFinished)`.
    - Provide `filteredJobs` to `TableView`, `BoardView`, and `pipelineLanes`.
    - Preserve detail panel for selected jobs: ensure `useJobDetails` receives a list including `selected` if present in `jobs` so that deep-linked or selected jobs are not lost when filtered out of the main view.
    - Add controls to `Header` `actions`:
      - Cloudscape `Toggle` ("Show finished jobs") controlling `showFinished`.
      - Cloudscape `Multiselect` with options from `STATE_ORDER`, controlling `selectedStates`, placeholder `"Filter by state"`.
    - Update `Header` counter to display `(${filteredJobs.length} of ${jobs.length})` when filtered and `(${jobs.length})` when unfiltered.
- In `web/src/views.tsx`:
  - Update empty messages in `TableView`, `BoardView`, and `PipelineView` so that when `jobs.length > 0` but filtered jobs is empty, the message reads `"No jobs match the current filters"` (otherwise `"No jobs yet"` when `jobs.length === 0`).

## Verify

- `cd web && bun run typecheck`
- `cd web && bun test`
- `cd web && bun run build`
- `cargo build -p pizza-delivery` from repo root.
- Interactive verification: verify toggle and multi-select in Table, Board, and Pipeline views, check empty state message when filtering out all jobs, and verify deep-link detail panel behavior.

## Done when

The controls render and filter jobs across all views, typecheck and builds pass cleanly.
