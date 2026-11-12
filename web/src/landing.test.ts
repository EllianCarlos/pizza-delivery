import { expect, test } from "bun:test";
import { hashFor, jobFromPath, landingRoute, routeFromHash } from "./model";

test("the phone landing page is the inbox and wider screens land on the home screen", () => {
  expect(landingRoute(390)).toBe("inbox");
  expect(landingRoute(600)).toBe("inbox");
  expect(landingRoute(1024)).toBe("home");
});

test("a notification link opens the job it names", () => {
  expect(jobFromPath("/jobs/01J9ABC")).toBe("01J9ABC");
  expect(jobFromPath("/jobs/01J9ABC/")).toBe("01J9ABC");
  expect(jobFromPath("/")).toBeNull();
  expect(jobFromPath("/api/jobs")).toBeNull();
});

test("views live under the agent workers section and round-trip through the hash", () => {
  expect(routeFromHash("")).toBe("home");
  expect(routeFromHash("#/")).toBe("home");
  expect(routeFromHash("#/agent-workers/events")).toBe("events");
  expect(routeFromHash("#/agent-workers/nope")).toBeNull();
  for (const r of ["home", "jobs", "inbox", "events", "artifacts"] as const) {
    expect(routeFromHash(hashFor(r))).toBe(r);
  }
});
