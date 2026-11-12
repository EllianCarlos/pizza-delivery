import { expect, test } from "bun:test";
import { createApi } from "./api";

type Call = { url: string; init?: RequestInit };

function fakeFetch(responses: Array<{ status: number; body: unknown }>, calls: Call[]) {
  return (async (url: string, init?: RequestInit) => {
    calls.push({ url, init });
    const next = responses.shift() ?? { status: 500, body: {} };
    return new Response(JSON.stringify(next.body), {
      status: next.status,
      headers: { "content-type": "application/json" },
    });
  }) as unknown as typeof fetch;
}

test("replying posts targets and text as json and returns the follow-up job", async () => {
  const calls: Call[] = [];
  const api = createApi(fakeFetch([{ status: 200, body: { id: "J2", short_id: "j2", answered: ["E1"] } }], calls));
  const out = await api.reply(["E1"], "use v2");
  expect(out.short_id).toBe("j2");
  expect(calls[0]?.url).toBe("/api/inbox/reply");
  expect(calls[0]?.init?.method).toBe("POST");
  expect(new Headers(calls[0]?.init?.headers).get("content-type")).toBe("application/json");
  expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({ targets: ["E1"], text: "use v2" });
});

test("dismiss, submit and cancel hit their endpoints", async () => {
  const calls: Call[] = [];
  const api = createApi(
    fakeFetch(
      [
        { status: 200, body: { entries: ["E1"] } },
        { status: 200, body: { id: "J3", short_id: "j3" } },
        { status: 200, body: { id: "J3", short_id: "j3" } },
      ],
      calls,
    ),
  );
  await api.dismiss(["E1"]);
  await api.submit("do it", "/repo", "high");
  await api.cancel("j3");
  expect(calls.map((c) => c.url)).toEqual(["/api/inbox/dismiss", "/api/jobs", "/api/jobs/j3/cancel"]);
  expect(JSON.parse(String(calls[1]?.init?.body))).toEqual({ prompt: "do it", repo: "/repo", priority: "high" });
});

test("an error response is raised with the server's message", async () => {
  const api = createApi(
    fakeFetch([{ status: 404, body: { kind: "not-found", message: "no such job", matches: [] } }], []),
  );
  await expect(api.job("zzz")).rejects.toThrow("no such job");
});

test("artifact content is fetched by position", async () => {
  const calls: Call[] = [];
  const api = createApi(fakeFetch([{ status: 200, body: { path: "a.md", content: "hi" } }], calls));
  expect((await api.artifact("j1", 2)).content).toBe("hi");
  expect(calls[0]?.url).toBe("/api/jobs/j1/artifacts/2");
});
