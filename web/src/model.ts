import type { EventView } from "./generated/EventView";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobState } from "./generated/JobState";
import type { JobView } from "./generated/JobView";
import type { StageState } from "./generated/StageState";

export const PHONE_MAX_WIDTH = 600;

export type Route = "home" | "jobs" | "inbox" | "events" | "artifacts";

/** The views grouped under the "Agent workers" section of the navigation. */
export const AGENT_WORKER_ROUTES = ["jobs", "inbox", "events", "artifacts"] as const;

export function landingRoute(width: number): Route {
  return width <= PHONE_MAX_WIDTH ? "inbox" : "home";
}

export function routeFromHash(hash: string): Route | null {
  if (hash === "" || hash === "#" || hash === "#/") return "home";
  const match = /^#\/agent-workers\/(jobs|inbox|events|artifacts)\/?$/.exec(hash);
  return (match?.[1] as Route | undefined) ?? null;
}

export function hashFor(route: Route): string {
  return route === "home" ? "#/" : `#/agent-workers/${route}`;
}

/** The routes linked from the top header; Artifacts stays side-nav only to keep the bar small. */
export const TOP_NAV_ROUTES = ["jobs", "inbox", "events"] as const;

export type TopNavItem =
  | { kind: "link"; text: string; href: string }
  | { kind: "enable-push"; text: "Enable notifications" };

const TOP_NAV_TEXT: Record<(typeof TOP_NAV_ROUTES)[number], string> = {
  jobs: "Jobs",
  inbox: "Inbox",
  events: "Events",
};

/** The top header's utilities: one link per TOP_NAV_ROUTES entry, then the push opt-in when offered. */
export function topNavItems(opts: { canEnablePush: boolean }): TopNavItem[] {
  const items: TopNavItem[] = TOP_NAV_ROUTES.map((route) => ({
    kind: "link",
    text: TOP_NAV_TEXT[route],
    href: hashFor(route),
  }));
  if (opts.canEnablePush) items.push({ kind: "enable-push", text: "Enable notifications" });
  return items;
}

export function jobFromPath(path: string): string | null {
  const match = /^\/jobs\/([^/]+)\/?$/.exec(path);
  return match?.[1] ?? null;
}

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

/** How an artifact reads in the job view: which stage made it and its file. */
export type ArtifactLabel = { stage: string; file: string; order: number };

/**
 * Splits a stored artifact path into the stage that produced it and a short
 * file name. Paths are `jobs/<job>/stages/<n>-<stage>/<file>`, or point into
 * the job's worktree (`worktrees/<repo>/<job>/<file>`), which reads as "repo".
 * Files at the job root (such as context.md) belong to no stage ("job").
 */
export function labelArtifact(path: string): ArtifactLabel {
  const parts = path.split("/");
  const stages = parts.indexOf("stages");
  if (parts[0] === "jobs" && stages === 2 && parts.length > 4) {
    const dir = parts[3] ?? "";
    const m = /^(\d+)-(.+)$/.exec(dir);
    return {
      stage: m?.[2] ?? dir,
      file: parts.slice(4).join("/"),
      order: m ? Number(m[1]) : 0,
    };
  }
  if (parts[0] === "worktrees" && parts.length > 3) {
    return { stage: "repo", file: parts.slice(3).join("/"), order: Number.MAX_SAFE_INTEGER };
  }
  if (parts[0] === "jobs" && parts.length > 2) {
    return { stage: "job", file: parts.slice(2).join("/"), order: -1 };
  }
  return { stage: "", file: path, order: 0 };
}
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

export const TOOL_KIND = "tool-called";

export function visibleEvents(
  events: EventView[],
  families: ReadonlySet<string>,
  showTools: boolean,
): EventView[] {
  const filtered = filterEvents(events, families);
  return showTools || families.has(eventFamily(TOOL_KIND))
    ? filtered
    : filtered.filter((e) => e.kind !== TOOL_KIND);
}

export type ToolCall = {
  cursor: number;
  ts_ms: number;
  stage_id: string | null;
  name: string;
  summary: string;
};

export function toolCallsOf(events: EventView[]): ToolCall[] {
  return events
    .filter((e) => e.kind === TOOL_KIND)
    .map((e) => {
      const p = (e.payload ?? {}) as { name?: unknown; summary?: unknown };
      return {
        cursor: e.cursor,
        ts_ms: e.ts_ms,
        stage_id: e.stage_id,
        name: typeof p.name === "string" ? p.name : "?",
        summary: typeof p.summary === "string" ? p.summary : "",
      };
    });
}

export function formatDuration(ms: number | null): string {
  if (ms === null) return "-";
  if (ms < 10_000) return `${(Math.max(ms, 0) / 1000).toFixed(1)}s`;
  const total = Math.floor(ms / 1000);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return m > 0 ? `${m}m ${total % 60}s` : `${total}s`;
}

export function formatCost(microusd: number | null): string {
  return microusd === null ? "-" : `$${(microusd / 1_000_000).toFixed(4)}`;
}

export function formatTokens(input: number | null, output: number | null): string {
  if (input === null && output === null) return "-";
  const n = (v: number | null) => (v === null ? "-" : v.toLocaleString("en-US"));
  return `${n(input)} in / ${n(output)} out`;
}

export type StageRow = {
  id: string;
  name: string;
  state: StageState;
  model: string | null;
  agent: string | null;
  wallMs: number | null;
  live: boolean;
  inputTokens: number | null;
  outputTokens: number | null;
  costMicrousd: number | null;
};

export function stageRows(detail: JobDetailView, nowMs: number): StageRow[] {
  return detail.stages.map((s) => {
    const live = s.state === "running";
    const started = [...detail.events]
      .reverse()
      .find((e) => e.kind === "stage-started" && e.stage_id === s.id);
    const u = live ? null : s.usage;
    return {
      id: s.id,
      name: s.name,
      state: s.state,
      model: u?.model ?? null,
      agent: u?.agent ?? null,
      wallMs: live ? (started ? Math.max(nowMs - started.ts_ms, 0) : null) : (u?.wall_ms ?? null),
      live,
      inputTokens: u?.input_tokens ?? null,
      outputTokens: u?.output_tokens ?? null,
      costMicrousd: u?.cost_microusd ?? null,
    };
  });
}

export type JobSummary = { wallMs: number | null; costMicrousd: number | null; models: string[] };

export function jobSummary(detail: JobDetailView, nowMs: number): JobSummary {
  const rows = stageRows(detail, nowMs);
  const total = (values: (number | null)[]) => {
    const known = values.filter((v): v is number => v !== null);
    return known.length === 0 ? null : known.reduce((a, b) => a + b, 0);
  };
  return {
    wallMs: total(rows.map((r) => r.wallMs)),
    costMicrousd: total(rows.map((r) => r.costMicrousd)),
    models: [...new Set(rows.flatMap((r) => (r.model ? [r.model] : [])))].sort(),
  };
}
