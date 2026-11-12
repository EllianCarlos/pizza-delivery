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
import type { TriageOptions } from "./options";

export type ArtifactContent = { path: string; content: string };

type Fetcher = (input: string, init?: RequestInit) => Promise<Response>;

export type SnapshotStorage = {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};

export type ApiOptions = { storage?: SnapshotStorage | null; now?: () => number };

const SNAPSHOT_PREFIX = "pizza-delivery:snapshot:";
const CLIENT_HEADERS = { "content-type": "application/json", "x-pizza-client": "web" };

function browserStorage(): SnapshotStorage | null {
  try {
    return globalThis.localStorage ?? null;
  } catch {
    return null;
  }
}

export function createApi(
  fetcher: Fetcher = (input, init) => fetch(input, init),
  options: ApiOptions = {},
) {
  const storage = options.storage === undefined ? browserStorage() : options.storage;
  const now = options.now ?? (() => Date.now());
  let stale: number | null = null;

  function remember(path: string, json: unknown) {
    try {
      storage?.setItem(SNAPSHOT_PREFIX + path, JSON.stringify({ at: now(), json }));
    } catch {
      return;
    }
  }

  function recall<T>(path: string): { at: number; json: T } | null {
    try {
      const raw = storage?.getItem(SNAPSHOT_PREFIX + path);
      return raw ? (JSON.parse(raw) as { at: number; json: T }) : null;
    } catch {
      return null;
    }
  }

  async function call<T>(path: string, body?: unknown): Promise<T> {
    const write = body !== undefined;
    if (write && stale !== null) throw new Error("offline: showing a read-only snapshot");
    const init: RequestInit = write
      ? { method: "POST", headers: CLIENT_HEADERS, body: JSON.stringify(body) }
      : {};
    let res: Response;
    try {
      res = await fetcher(path, init);
    } catch (e) {
      if (write) throw new Error("offline: the request could not be sent");
      const snapshot = recall<T>(path);
      if (!snapshot) throw new Error("offline and no snapshot is available");
      stale = snapshot.at;
      return snapshot.json;
    }
    const json = (await res.json()) as T | ApiError;
    if (!res.ok) throw new Error((json as ApiError).message ?? `request failed: ${res.status}`);
    if (!write) {
      stale = null;
      remember(path, json);
    }
    return json as T;
  }

  const post = (path: string) => call<SubmittedView>(path, {});
  return {
    staleSince: () => stale,
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
    submit: (prompt: string, repo?: string, priority?: Priority, options?: TriageOptions) =>
      call<SubmittedView>("/api/jobs", {
        prompt,
        ...(repo ? { repo } : {}),
        ...(priority ? { priority } : {}),
        ...(options ? { options } : {}),
      }),
    cancel: (ref: string) => post(`/api/jobs/${ref}/cancel`),
    pushKey: () => call<{ key: string }>("/api/push/key"),
    pushSubscribe: (subscription: PushSubscriptionJSON) =>
      call<{ subscribed: boolean }>("/api/push/subscribe", subscription),
  };
}

export type Api = ReturnType<typeof createApi>;
