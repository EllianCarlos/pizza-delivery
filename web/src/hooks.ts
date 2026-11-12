import { useCallback, useEffect, useRef, useState } from "react";
import { detailsToFetch, mergeEvents, wantedJobs } from "./fetching";
import type { EventsView } from "./generated/EventsView";
import type { EventView } from "./generated/EventView";
import type { JobFullView } from "./generated/JobFullView";
import type { JobView } from "./generated/JobView";

/** `deps` re-runs the load immediately when they change (e.g. a filter). */
export function usePolled<T>(load: () => Promise<T>, ms = 3000, deps: readonly unknown[] = []) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  const refresh = useCallback(async () => {
    try {
      setData(await loadRef.current());
      setError(null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), ms);
    return () => clearInterval(timer);
  }, [refresh, ms, ...deps]);
  return { data, error, refresh };
}

/**
 * Events already in the ledger, then live ones over a single stream opened after them.
 * One fetch and one connection for the life of the component: the stream resumes on its
 * own after a drop, so there is no need to poll the list again.
 */
export function useLiveEvents(load: () => Promise<EventsView>, limit = 500) {
  const [events, setEvents] = useState<EventView[]>([]);
  const [since, setSince] = useState<number | null>(null);
  const loadRef = useRef(load);
  loadRef.current = load;
  useEffect(() => {
    let alive = true;
    loadRef
      .current()
      .then((view) => {
        if (!alive) return;
        setEvents(view.events.slice(-limit));
        setSince(view.next);
      })
      .catch(() => alive && setSince(0));
    return () => {
      alive = false;
    };
  }, [limit]);
  useEffect(() => {
    if (since === null) return;
    const source = new EventSource(`/api/stream?since=${since}`);
    const onMessage = (m: MessageEvent<string>) => {
      const event = JSON.parse(m.data) as EventView;
      setEvents((prev) => mergeEvents(prev, event, limit));
    };
    for (const kind of KINDS) source.addEventListener(kind, onMessage as EventListener);
    return () => source.close();
  }, [since, limit]);
  return events;
}

/**
 * Details of the jobs the current view needs, fetched through the one bundled endpoint.
 * Jobs that ended are fetched once; only moving ones are polled. `refresh` forces a reload.
 */
export function useJobDetails(
  fetchDetail: (ref: string) => Promise<JobFullView>,
  jobs: JobView[],
  selected: string | null,
  all: boolean,
  ms = 3000,
) {
  const cache = useRef(new Map<string, JobFullView>());
  const force = useRef(false);
  const wanted = wantedJobs(jobs, selected, all);
  const key = wanted.map((j) => `${j.id}:${j.state}`).join(",");
  const poll = usePolled<JobFullView[]>(async () => {
    const seen = new Map([...cache.current].map(([id, d]) => [id, d.job.state]));
    const stale = force.current ? wanted : detailsToFetch(wanted, seen);
    force.current = false;
    const fetched = await Promise.all(stale.map((j) => fetchDetail(j.short_id)));
    for (const detail of fetched) cache.current.set(detail.job.id, detail);
    return wanted.flatMap((j) => cache.current.get(j.id) ?? []);
  }, ms);
  const first = useRef(key);
  useEffect(() => {
    if (first.current === key) return;
    first.current = key;
    void poll.refresh();
  }, [key, poll.refresh]);
  const refresh = useCallback(async () => {
    force.current = true;
    await poll.refresh();
  }, [poll.refresh]);
  return { data: poll.data, error: poll.error, refresh };
}

const KINDS = [
  "job-submitted",
  "job-completed",
  "job-failed",
  "job-cancelled",
  "job-parked",
  "job-needs-human",
  "stage-planned",
  "stage-claimed",
  "stage-started",
  "stage-succeeded",
  "stage-failed",
  "stage-retried",
  "stage-budget-recorded",
  "agent-invoked",
  "agent-finished",
  "tool-called",
  "agent-fell-back",
  "quota-signal",
  "sandbox-audit",
  "egress-blocked",
  "artifact-recorded",
  "handoff-written",
  "lease-expired",
  "lease-released",
  "context-read",
  "triage-completed",
  "feedback-recorded",
  "feedback-dismissed",
  "plan-amended",
  "budget-estimated",
  "mcp-requested",
  "mcp-activated",
  "mcp-denied",
  "mcp-deactivated",
  "mcp-calls",
  "write-back",
  "remote-action",
  "auth-rejected",
];
