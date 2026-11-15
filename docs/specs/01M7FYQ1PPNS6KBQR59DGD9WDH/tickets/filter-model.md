---
id: filter-model
blocked_by: []
---

# Pure `visibleJobs` helper and unit tests in `model.ts`

Implement the job visibility and filtering helper in `web/src/model.ts` and cover all permutations in `web/src/model.test.ts`.

## Do

- In `web/src/model.ts`:
  - Define `FINISHED_JOB_STATES: readonly JobState[] = ["completed", "failed", "cancelled"] as const`.
  - Export `isFinishedJobState(state: JobState): boolean`.
  - Export `visibleJobs(jobs: JobView[], states: ReadonlySet<JobState> | readonly JobState[], showFinished: boolean): JobView[]`.
  - Logic:
    - If `states` is non-empty (set size > 0 or array length > 0), return jobs where `job.state` is in `states`.
    - If `states` is empty:
      - If `showFinished` is true, return all `jobs`.
      - If `showFinished` is false, return jobs where `!FINISHED_JOB_STATES.includes(job.state)`.
- In `web/src/model.test.ts`:
  - Add unit tests for `visibleJobs`:
    - Default view (`states: []`, `showFinished: false`): hides `completed`, `failed`, `cancelled` jobs, retains `queued`, `running`, `waiting-for-quota`, `needs-human`.
    - Show finished enabled (`states: []`, `showFinished: true`): returns all jobs.
    - Explicit state filter for finished state (`states: ["completed"]`, `showFinished: false`): returns completed jobs (overrides selector).
    - Multi-state filter (`states: ["running", "failed"]`): returns only jobs matching those states.
    - Empty input jobs array: returns empty array.
    - No matches: returns empty array.

## Verify

- Run `cd web && bun test src/model.test.ts` to ensure all tests pass.
- Run `cd web && bun run typecheck` to verify TypeScript types.

## Done when

All model tests for `visibleJobs` pass and typecheck succeeds.
