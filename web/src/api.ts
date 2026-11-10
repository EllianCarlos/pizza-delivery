import type { ApiError } from "./generated/ApiError";
import type { DismissedView } from "./generated/DismissedView";
import type { EventsView } from "./generated/EventsView";
import type { InboxView } from "./generated/InboxView";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobsView } from "./generated/JobsView";
import type { Priority } from "./generated/Priority";
import type { QuotaPanelView } from "./generated/QuotaPanelView";
import type { RepliedView } from "./generated/RepliedView";
import type { SubmittedView } from "./generated/SubmittedView";

export type ArtifactContent = { path: string; content: string };

type Fetcher = (input: string, init?: RequestInit) => Promise<Response>;

export function createApi(fetcher: Fetcher = (input, init) => fetch(input, init)) {
  async function call<T>(path: string, body?: unknown): Promise<T> {
    const init: RequestInit =
      body === undefined
        ? {}
        : {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(body),
          };
    const res = await fetcher(path, init);
    const json = (await res.json()) as T | ApiError;
    if (!res.ok) throw new Error((json as ApiError).message ?? `request failed: ${res.status}`);
    return json as T;
  }
  const post = (path: string) => call<SubmittedView>(path, {});
  return {
    jobs: () => call<JobsView>("/api/jobs"),
    job: (ref: string) => call<JobDetailView>(`/api/jobs/${ref}`),
    quota: () => call<QuotaPanelView>("/api/quota"),
    inbox: (all = false) => call<InboxView>(all ? "/api/inbox?all=1" : "/api/inbox"),
    events: (since = 0) => call<EventsView>(`/api/events?since=${since}`),
    artifact: (ref: string, index: number) =>
      call<ArtifactContent>(`/api/jobs/${ref}/artifacts/${index}`),
    reply: (targets: string[], text: string, priority?: Priority) =>
      call<RepliedView>("/api/inbox/reply", { targets, text, ...(priority ? { priority } : {}) }),
    dismiss: (entries: string[]) => call<DismissedView>("/api/inbox/dismiss", { entries }),
    submit: (prompt: string, repo?: string, priority?: Priority) =>
      call<SubmittedView>("/api/jobs", {
        prompt,
        ...(repo ? { repo } : {}),
        ...(priority ? { priority } : {}),
      }),
    cancel: (ref: string) => post(`/api/jobs/${ref}/cancel`),
  };
}

export type Api = ReturnType<typeof createApi>;
