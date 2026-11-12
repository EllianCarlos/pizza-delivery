import type { EventView } from "./generated/EventView";
import type { JobState } from "./generated/JobState";
import type { JobView } from "./generated/JobView";

const FINAL: JobState[] = ["completed", "failed", "cancelled"];

/**
 * Jobs whose detail must be fetched: never fetched, still moving, or in a different state
 * than when it was cached. A job that ended and was cached in that state cannot change.
 */
export function detailsToFetch(jobs: JobView[], cached: ReadonlyMap<string, JobState>): JobView[] {
  return jobs.filter((j) => {
    const seen = cached.get(j.id);
    return seen === undefined || seen !== j.state || !FINAL.includes(j.state);
  });
}

/** Details are needed for every job only by the pipeline view; otherwise for the open one. */
export function wantedJobs(jobs: JobView[], selected: string | null, all: boolean): JobView[] {
  return all ? jobs : jobs.filter((j) => j.id === selected);
}

/** Appends a streamed event unless its cursor was already seen, keeping the newest `limit`. */
export function mergeEvents(prev: EventView[], event: EventView, limit: number): EventView[] {
  const last = prev[prev.length - 1]?.cursor ?? 0;
  if (event.cursor <= last) return prev;
  return [...prev, event].slice(-limit);
}
