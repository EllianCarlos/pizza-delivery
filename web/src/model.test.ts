import { expect, test } from "bun:test";
import type { EventView } from "./generated/EventView";
import type { JobView } from "./generated/JobView";
import type { JobDetailView } from "./generated/JobDetailView";
import {
  artifactsOf,
  eventFamilies,
  eventFamily,
  filterEvents,
  groupByState,
  pipelineLanes,
  segmentsOf,
} from "./model";

const ev = (cursor: number, kind: string, payload: unknown = {}): EventView => ({
  cursor,
  id: `E${cursor}`,
  job_id: "J1",
  stage_id: null,
  seq: cursor,
  ts_ms: cursor,
  actor: "daemon",
  kind,
  payload,
});

const job = (id: string, state: JobView["state"]): JobView => ({
  id,
  short_id: id.toLowerCase(),
  state,
  prompt: `prompt ${id}`,
  submitted_at_ms: 1,
});

test("the event family is the first word of the kind", () => {
  expect(eventFamily("stage-started")).toBe("stage");
  expect(eventFamily("quota-signal")).toBe("quota");
  expect(eventFamily("job-submitted")).toBe("job");
});

test("families are listed once, sorted, from the events present", () => {
  const events = [ev(1, "stage-started"), ev(2, "job-submitted"), ev(3, "stage-succeeded")];
  expect(eventFamilies(events)).toEqual(["job", "stage"]);
});

test("filtering by families keeps only matching events and none selected keeps all", () => {
  const events = [ev(1, "stage-started"), ev(2, "job-submitted"), ev(3, "quota-signal")];
  expect(filterEvents(events, new Set(["stage", "quota"])).map((e) => e.cursor)).toEqual([1, 3]);
  expect(filterEvents(events, new Set()).map((e) => e.cursor)).toEqual([1, 2, 3]);
});

test("artifacts are numbered by their position among the job's artifact events", () => {
  const events = [
    ev(1, "stage-started"),
    ev(2, "artifact-recorded", { kind: "spec", path: "jobs/J1/spec.md", sha256: "aa" }),
    ev(3, "artifact-recorded", { kind: "handoff", path: "jobs/J1/handoff.md", sha256: "bb" }),
  ];
  expect(artifactsOf(events)).toEqual([
    { index: 0, kind: "spec", path: "jobs/J1/spec.md", sha256: "aa" },
    { index: 1, kind: "handoff", path: "jobs/J1/handoff.md", sha256: "bb" },
  ]);
});

test("jobs are grouped into one column per state in a fixed order, empty columns kept", () => {
  const columns = groupByState([job("A", "running"), job("B", "queued"), job("C", "running")]);
  expect(columns.map((c) => c.state)).toEqual([
    "queued",
    "running",
    "waiting-for-quota",
    "needs-human",
    "completed",
    "failed",
    "cancelled",
  ]);
  expect(columns.find((c) => c.state === "running")?.jobs.map((j) => j.id)).toEqual(["A", "C"]);
  expect(columns.find((c) => c.state === "failed")?.jobs).toEqual([]);
});

test("a lane has one segment per planned stage in plan order, with run state when it exists", () => {
  const detail = {
    job: job("A", "running"),
    plan: {
      stages: [
        { name: "triage", reason: "r", tier: null, estimated_tokens: null, estimated_wall_ms: null },
        { name: "implement", reason: "r", tier: null, estimated_tokens: null, estimated_wall_ms: null },
        { name: "review", reason: "r", tier: null, estimated_tokens: null, estimated_wall_ms: null },
      ],
    },
    stages: [
      { id: "S1", n: 1, name: "triage", state: "succeeded", attempt: 1, usage: null },
      { id: "S2", n: 2, name: "implement", state: "running", attempt: 1, usage: null },
    ],
    feedback: [],
    events: [],
  } satisfies JobDetailView;
  expect(segmentsOf(detail)).toEqual([
    { name: "triage", state: "succeeded" },
    { name: "implement", state: "running" },
    { name: "review", state: "pending" },
  ]);
  expect(pipelineLanes([detail])).toEqual([{ job: detail.job, segments: segmentsOf(detail) }]);
});
