import { expect, test } from "bun:test";
import { createApi } from "./api";
import { inboxPath } from "./inbox";

test("no filter asks for the server's default inbox", () => {
  expect(inboxPath({})).toBe("/api/inbox");
  expect(inboxPath({ status: "active", q: "   " })).toBe("/api/inbox");
});

test("a status filter and a text query become query parameters", () => {
  expect(inboxPath({ status: "dismissed" })).toBe("/api/inbox?status=dismissed");
  expect(inboxPath({ status: "all" })).toBe("/api/inbox?all=1");
  expect(inboxPath({ status: "open", q: "api v2 & more" })).toBe(
    "/api/inbox?status=open&q=api%20v2%20%26%20more",
  );
  expect(inboxPath({ q: " risk " })).toBe("/api/inbox?q=risk");
});

test("the api fetches the filtered inbox", async () => {
  const urls: string[] = [];
  const api = createApi(
    (async (url: string) => {
      urls.push(url);
      return new Response(JSON.stringify({ entries: [] }), { status: 200 });
    }) as unknown as typeof fetch,
    { storage: null },
  );
  await api.inbox({ status: "answered", q: "db" });
  expect(urls).toEqual(["/api/inbox?status=answered&q=db"]);
});
