# Finished Jobs Selector and Job State Filter in Jobs Page

## Problem

In the pizza delivery web UI (`web/src/App.tsx`), `JobsPage` renders the complete, unfiltered array of jobs fetched from the API across all three view modes: `TableView`, `BoardView`, and `PipelineView` (`web/src/views.tsx`). As jobs are submitted, executed, and terminated, jobs in finished states (`completed`, `failed`, `cancelled`) accumulate indefinitely in the UI.

Users navigating the Jobs page typically want to focus on active or moving work that requires attention or is currently progressing (specifically `needs-human`, `running`, `queued`, and `waiting-for-quota`). Having to scan through numerous terminal jobs creates visual noise, reduces productivity, and clutters the board and table.

Furthermore, there is currently no mechanism to filter jobs by their specific state (such as viewing only `needs-human` jobs that require operator intervention, or only `failed` jobs to investigate errors).

The task is:
"In the pizza delivery app, for every view in jobs, create a selector that hides or shows finishes as well as filter for the state of each job. The selector defaults to no (shows only needs-human and running and similars) and the filter starts empty."

## Solution

The solution introduces pure filtering logic in `web/src/model.ts`, corresponding unit tests in `web/src/model.test.ts`, and UI controls in `JobsPage` in `web/src/App.tsx` that apply uniformly to all three views (`TableView`, `BoardView`, and `PipelineView`), as well as documentation in `GLOSSARY.md`.

### 1. Pure Filtering Logic (`web/src/model.ts`)
- Define `FINISHED_JOB_STATES: readonly JobState[] = ["completed", "failed", "cancelled"] as const`.
- Export a pure helper function:
  ```ts
  export function visibleJobs(
    jobs: JobView[],
    states: ReadonlySet<JobState> | readonly JobState[],
    showFinished: boolean,
  ): JobView[]
  ```
- Filtering semantics:
  - **Explicit state filter**: If `states` is non-empty (one or more states selected), return jobs whose `state` is included in `states`. When a user explicitly chooses a state (e.g. `completed` or `failed`), those jobs are displayed regardless of `showFinished`, ensuring an explicit filter is never silently blank.
  - **Default / empty state filter**: If `states` is empty:
    - When `showFinished` is `false` (default): return only non-finished jobs (`queued`, `running`, `waiting-for-quota`, `needs-human`). Finished jobs (`completed`, `failed`, `cancelled`) are excluded.
    - When `showFinished` is `true`: return all jobs without restriction.
- Export `isFinishedJobState(state: JobState): boolean` for convenient querying.

### 2. UI Controls in `JobsPage` (`web/src/App.tsx`)
- In `JobsPage`, maintain local component state:
  - `const [showFinished, setShowFinished] = useState(false);`
  - `const [selectedStates, setSelectedStates] = useState<JobState[]>([]);`
- Derive `filteredJobs` using `useMemo(() => visibleJobs(jobs, new Set(selectedStates), showFinished), [jobs, selectedStates, showFinished])`.
- In `Header` `actions`, place controls in an aligned horizontal layout alongside the existing view switcher (`SegmentedControl`) and "Submit job" `Button`:
  - A Cloudscape `Toggle` labelled "Show finished jobs" bound to `showFinished`.
  - A Cloudscape `Multiselect` with options constructed from `STATE_ORDER`, bound to `selectedStates`, placeholder `"Filter by state"`.
- Update `Header` counter to show `(${filteredJobs.length} of ${jobs.length})` when filters hide jobs, and `(${jobs.length})` when all jobs are visible.
- Detail fetching & selection preservation:
  - Pass `filteredJobs` to `TableView`, `BoardView`, and `pipelineLanes`.
  - For `useJobDetails`, construct `jobsForDetails`: if `selected` is present in `jobs` but filtered out of `filteredJobs`, include it in `jobsForDetails` so `useJobDetails` fetches and caches its details, keeping deep-linked (`jobFromPath`) and previously selected job detail panels functional.

### 3. Empty States in Views (`web/src/views.tsx`)
- Update `TableView`, `BoardView`, and `PipelineView` empty states: when `jobs.length > 0` but no jobs match the active filters, display `"No jobs match the current filters"` instead of `"No jobs yet"`.

### 4. Domain Glossary (`GLOSSARY.md`)
- Add definitions for `Finished jobs` and `Job state filter` to `GLOSSARY.md`.

## Acceptance criteria

1. **Filtering Logic**:
   - `visibleJobs(jobs, [], false)` (or empty set) excludes all jobs with state `completed`, `failed`, or `cancelled`, returning only jobs with state `queued`, `running`, `waiting-for-quota`, or `needs-human`.
   - `visibleJobs(jobs, [], true)` includes all jobs regardless of state.
   - `visibleJobs(jobs, ["completed"], false)` returns completed jobs (an explicit state filter overrides the finished toggle).
   - `visibleJobs(jobs, ["running", "failed"], false)` returns running and failed jobs.
   - `visibleJobs([], ...)` returns an empty array.
2. **Web UI Behavior**:
   - The Jobs page header contains a "Show finished jobs" toggle (default: unchecked/off) and a "Filter by state" multi-select dropdown (default: empty).
   - Changing the toggle or state filter immediately filters jobs across Table, Board, and Pipeline views.
   - If jobs exist but all are filtered out, the view renders `"No jobs match the current filters"`.
   - If no jobs exist at all, the view renders `"No jobs yet"`.
   - The header counter displays `(X of Y)` when filtered, or `(Y)` when unfiltered.
   - A deep-linked job (via `/jobs/<job-id>`) displays its detail panel even if its state is currently hidden by default or active filters.
3. **Verification**:
   - `bun test` in `web/` passes and includes comprehensive unit tests for `visibleJobs` in `web/src/model.test.ts`.
   - `bun run typecheck` in `web/` passes with no type errors.
   - `bun run build` in `web/` succeeds without errors or warnings.
   - `cargo build -p pizza-delivery` from the repository root succeeds.
4. **Documentation**:
   - `GLOSSARY.md` contains entries for "Finished jobs" and "Job state filter".

## Seams

- **Model Unit Test Seam (`web/src/model.test.ts`)**:
  Automated unit test suite executed with `bun test`. Covers `visibleJobs` and state predicates with test cases for defaults, toggle toggling, single/multi-state filtering, mixed finished/active states, and empty edge cases.
- **TypeScript Typecheck & Build Seam (`web/`)**:
  Executed with `bun run typecheck` and `bun run build`. Verifies type validity of Cloudscape `Toggle` and `Multiselect` prop bindings, `JobState` enum conversions, and bundling.
- **UI / Visual Inspection Seam (`web/src/App.tsx`, `web/src/views.tsx`)**:
  Interactive verification in the browser verifying that toggling and multi-selecting updates Table, Board, and Pipeline views uniformly, updates the header counter, preserves deep links, and displays appropriate empty states.
