import { useCallback, useEffect, useRef, useState } from "react";
import type { EventView } from "./generated/EventView";

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

export function useEventStream(initial: EventView[], limit = 500) {
  const [events, setEvents] = useState<EventView[]>(initial);
  useEffect(() => setEvents(initial), [initial]);
  useEffect(() => {
    const last = initial[initial.length - 1]?.cursor ?? 0;
    const source = new EventSource(`/api/stream?since=${last}`);
    const onMessage = (m: MessageEvent<string>) => {
      const event = JSON.parse(m.data) as EventView;
      setEvents((prev) => [...prev, event].slice(-limit));
    };
    for (const kind of KINDS) source.addEventListener(kind, onMessage as EventListener);
    return () => source.close();
  }, [initial, limit]);
  return events;
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
