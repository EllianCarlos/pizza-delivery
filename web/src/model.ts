import type { EventView } from "./generated/EventView";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobState } from "./generated/JobState";
import type { JobView } from "./generated/JobView";
import type { StageState } from "./generated/StageState";

export const STATE_ORDER: JobState[] = [
  "queued",
  "running",
  "waiting-for-quota",
  "needs-human",
  "completed",
  "failed",
  "cancelled",
];

export type Column = { state: JobState; jobs: JobView[] };
export type Artifact = { index: number; kind: string; path: string; sha256: string };
export type Segment = { name: string; state: StageState | "pending" };
export type Lane = { job: JobView; segments: Segment[] };

export function eventFamily(kind: string): string {
  return kind.split("-")[0] ?? kind;
}

export function eventFamilies(events: EventView[]): string[] {
  return [...new Set(events.map((e) => eventFamily(e.kind)))].sort();
}

export function filterEvents(events: EventView[], families: ReadonlySet<string>): EventView[] {
  if (families.size === 0) return events;
  return events.filter((e) => families.has(eventFamily(e.kind)));
}

export function artifactsOf(events: EventView[]): Artifact[] {
  return events
    .filter((e) => e.kind === "artifact-recorded")
    .map((e, index) => {
      const p = e.payload as { kind: string; path: string; sha256: string };
      return { index, kind: p.kind, path: p.path, sha256: p.sha256 };
    });
}

export function groupByState(jobs: JobView[]): Column[] {
  return STATE_ORDER.map((state) => ({ state, jobs: jobs.filter((j) => j.state === state) }));
}

export function segmentsOf(detail: JobDetailView): Segment[] {
  return detail.plan.stages.map((planned) => {
    const run = [...detail.stages].reverse().find((s) => s.name === planned.name);
    return { name: planned.name, state: run ? run.state : "pending" };
  });
}

export function pipelineLanes(details: JobDetailView[]): Lane[] {
  return details.map((d) => ({ job: d.job, segments: segmentsOf(d) }));
}
