import { expect, test } from "bun:test";
import { createApi } from "./api";
import { previewSummary, type TriagePreview } from "./triage";

const resolved: TriagePreview = {
  outcome: {
    triage: {
      repo: { path: "/r/app", confidence: 90, alternatives: [{ name: "lib", path: "/r/lib", score: 40 }] },
      template: "bug-fix",
      plan: [
        { name: "spec", reason: "scope it" },
        { name: "build", reason: "do it" },
      ],
      risk_flags: ["production"],
      task: "Fix the toggle",
    },
    feedback: [],
  },
  context_sources: [{ source: "engram", bytes: 120, truncated: false }],
};

test("a resolved preview lists the repo, template, plan and risks", () => {
  const s = previewSummary(resolved);
  expect(s.resolved).toBe(true);
  expect(s.repo).toBe("/r/app (confidence 90%)");
  expect(s.template).toBe("bug-fix");
  expect(s.stages).toEqual([
    { n: 1, name: "spec", reason: "scope it" },
    { n: 2, name: "build", reason: "do it" },
  ]);
  expect(s.risks).toEqual(["production"]);
  expect(s.alternatives).toEqual(["/r/lib (40%)"]);
  expect(s.sources).toEqual(["engram (120 bytes)"]);
});

test("an unresolved preview carries the reason instead of a plan", () => {
  const s = previewSummary({
    outcome: { triage: null, feedback: [{ kind: "needs-human", severity: "high", text: "which repo?", confidence: null }] },
    context_sources: [],
  });
  expect(s.resolved).toBe(false);
  expect(s.notes).toEqual(["needs-human (high): which repo?"]);
  expect(s.stages).toEqual([]);
});

test("preview posts the submit body to the dry-run route and never to jobs", async () => {
  const calls: Array<{ url: string; init?: RequestInit }> = [];
  const api = createApi(
    (async (url: string, init?: RequestInit) => {
      calls.push({ url, init });
      return new Response(JSON.stringify(resolved), { status: 200 });
    }) as unknown as typeof fetch,
    { storage: null },
  );
  const out = await api.preview("fix it", "/r/app", { max_stages: 2 });
  expect(out.outcome.triage?.template).toBe("bug-fix");
  expect(calls[0]?.url).toBe("/api/dry-run");
  expect(calls[0]?.init?.method).toBe("POST");
  expect(new Headers(calls[0]?.init?.headers).get("x-pizza-client")).toBe("web");
  expect(JSON.parse(String(calls[0]?.init?.body))).toEqual({
    prompt: "fix it",
    repo: "/r/app",
    options: { max_stages: 2 },
  });
});

test("a rejected preview raises the server's message", async () => {
  const api = createApi(
    (async () =>
      new Response(JSON.stringify({ kind: "usage", message: "prompt must not be empty", matches: [] }), {
        status: 400,
      })) as unknown as typeof fetch,
    { storage: null },
  );
  await expect(api.preview("")).rejects.toThrow("prompt must not be empty");
});
