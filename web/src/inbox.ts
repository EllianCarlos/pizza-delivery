export type InboxStatus = "active" | "open" | "answered" | "dismissed" | "all";

export interface InboxQuery {
  status?: InboxStatus;
  q?: string;
}

/**
 * Request path for a filtered inbox. "active" (open and answered) is the
 * server's default, so it adds no parameter and the URL matches the unfiltered one.
 */
export function inboxPath({ status = "active", q = "" }: InboxQuery): string {
  const params: string[] = [];
  if (status === "all") params.push("all=1");
  else if (status !== "active") params.push(`status=${status}`);
  const text = q.trim();
  if (text !== "") params.push(`q=${encodeURIComponent(text)}`);
  return params.length > 0 ? `/api/inbox?${params.join("&")}` : "/api/inbox";
}
