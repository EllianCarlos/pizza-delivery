import { expect, test } from "bun:test";
import type { EventView } from "./generated/EventView";
import type { JobView } from "./generated/JobView";
import type { JobDetailView } from "./generated/JobDetailView";
import type { StageView } from "./generated/StageView";
import {
  artifactsOf,
  eventFamilies,
  eventFamily,
  filterEvents,
  formatCost,
  formatDuration,
  formatTokens,
  groupByState,
  hashFor,
  jobSummary,
  pipelineLanes,
  routeFromHash,
  segmentsOf,
  stageRows,
  TOP_NAV_ROUTES,
  toolCallsOf,
  topNavItems,
  visibleEvents,
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

const usage = (over: Partial<NonNullable<StageView["usage"]>> = {}): NonNullable<StageView["usage"]> => ({
  model: "claude-sonnet-5-5",
  input_tokens: 1200,
  output_tokens: 300,
  cost_microusd: 25_000,
  wall_ms: 65_000,
  agent: "claude",
  candidate_position: 0,
  ...over,
});

const stage = (id: string, name: string, state: StageView["state"], u: StageView["usage"]): StageView => ({
  id,
  n: 1,
  name,
  state,
  attempt: 1,
  usage: u,
});

const detailOf = (stages: StageView[], events: EventView[]): JobDetailView => ({
  job: job("A", "running"),
  plan: { stages: [] },
  stages,
  feedback: [],
  events,
});

test("durations, costs and tokens are formatted compactly with a dash for unknown", () => {
  expect(formatDuration(null)).toBe("-");
  expect(formatDuration(800)).toBe("0.8s");
  expect(formatDuration(65_000)).toBe("1m 5s");
  expect(formatDuration(3_725_000)).toBe("1h 2m");
  expect(formatCost(null)).toBe("-");
  expect(formatCost(25_000)).toBe("$0.0250");
  expect(formatTokens(null, null)).toBe("-");
  expect(formatTokens(1200, 300)).toBe("1,200 in / 300 out");
  expect(formatTokens(1200, null)).toBe("1,200 in / - out");
});

test("a finished stage shows its recorded usage", () => {
  const rows = stageRows(detailOf([stage("S1", "spec", "succeeded", usage())], []), 999_999);
  expect(rows).toEqual([
    {
      id: "S1",
      name: "spec",
      state: "succeeded",
      model: "claude-sonnet-5-5",
      agent: "claude",
      wallMs: 65_000,
      live: false,
      inputTokens: 1200,
      outputTokens: 300,
      costMicrousd: 25_000,
    },
  ]);
});

test("a running stage shows live elapsed time from its latest stage-started event", () => {
  const events = [
    { ...ev(1, "stage-started"), stage_id: "S1", ts_ms: 1_000 },
    { ...ev(2, "stage-started"), stage_id: "S1", ts_ms: 4_000 },
    { ...ev(3, "stage-started"), stage_id: "S2", ts_ms: 2_000 },
  ];
  const rows = stageRows(detailOf([stage("S1", "implement", "running", null)], events), 10_000);
  expect(rows[0]).toMatchObject({ wallMs: 6_000, live: true, model: null, agent: null, costMicrousd: null });
});

test("a running stage without a start event has unknown wall time", () => {
  const rows = stageRows(detailOf([stage("S1", "implement", "running", null)], []), 10_000);
  expect(rows[0]).toMatchObject({ wallMs: null, live: true });
});

test("the job summary totals time and cost and lists the distinct models", () => {
  const stages = [
    stage("S1", "spec", "succeeded", usage()),
    stage("S2", "implement", "succeeded", usage({ model: "claude-opus", cost_microusd: 75_000, wall_ms: 5_000 })),
    stage("S3", "review", "succeeded", usage({ model: "claude-sonnet-5-5", cost_microusd: null, wall_ms: null })),
  ];
  expect(jobSummary(detailOf(stages, []), 0)).toEqual({
    wallMs: 70_000,
    costMicrousd: 100_000,
    models: ["claude-opus", "claude-sonnet-5-5"],
  });
  expect(jobSummary(detailOf([], []), 0)).toEqual({ wallMs: null, costMicrousd: null, models: [] });
});

test("tool calls are read from tool-called events, tolerating odd payloads", () => {
  const events = [
    ev(1, "stage-started"),
    { ...ev(2, "tool-called", { name: "Read", summary: "/a.rs" }), stage_id: "S1" },
    ev(3, "tool-called", {}),
    ev(4, "tool-called", null),
  ];
  expect(toolCallsOf(events)).toEqual([
    { cursor: 2, ts_ms: 2, stage_id: "S1", name: "Read", summary: "/a.rs" },
    { cursor: 3, ts_ms: 3, stage_id: null, name: "?", summary: "" },
    { cursor: 4, ts_ms: 4, stage_id: null, name: "?", summary: "" },
  ]);
  expect(toolCallsOf([])).toEqual([]);
});

test("tool-called events are hidden unless shown or their family is filtered", () => {
  const events = [ev(1, "stage-started"), ev(2, "tool-called"), ev(3, "agent-finished")];
  expect(visibleEvents(events, new Set(), false).map((e) => e.cursor)).toEqual([1, 3]);
  expect(visibleEvents(events, new Set(), true).map((e) => e.cursor)).toEqual([1, 2, 3]);
  expect(visibleEvents(events, new Set(["tool"]), false).map((e) => e.cursor)).toEqual([2]);
  expect(visibleEvents(events, new Set(["stage"]), true).map((e) => e.cursor)).toEqual([1]);
});

test("topNavItems without push returns Jobs, Inbox, Events links in order", () => {
  expect(topNavItems({ canEnablePush: false })).toEqual([
    { kind: "link", text: "Jobs", href: "#/agent-workers/jobs" },
    { kind: "link", text: "Inbox", href: "#/agent-workers/inbox" },
    { kind: "link", text: "Events", href: "#/agent-workers/events" },
  ]);
});

test("topNavItems with push appends a trailing enable-push item", () => {
  const items = topNavItems({ canEnablePush: true });
  expect(items.slice(0, 3)).toEqual(topNavItems({ canEnablePush: false }));
  expect(items).toHaveLength(4);
  expect(items[3]).toEqual({ kind: "enable-push", text: "Enable notifications" });
});

test("topNavItems link hrefs round-trip through routeFromHash", () => {
  const links = topNavItems({ canEnablePush: true }).flatMap((i) => (i.kind === "link" ? [i] : []));
  expect(links).toHaveLength(TOP_NAV_ROUTES.length);
  links.forEach((item, i) => {
    const route = TOP_NAV_ROUTES[i]!;
    expect(item.href).toBe(hashFor(route));
    expect(routeFromHash(item.href)).toBe(route);
  });
});
