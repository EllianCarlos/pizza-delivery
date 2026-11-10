import { expect, test } from "bun:test";
import { jobFromPath, landingTab } from "./model";

test("the phone landing page is the inbox and wider screens land on the jobs", () => {
  expect(landingTab(390)).toBe("inbox");
  expect(landingTab(600)).toBe("inbox");
  expect(landingTab(1024)).toBe("jobs");
});

test("a notification link opens the job it names", () => {
  expect(jobFromPath("/jobs/01J9ABC")).toBe("01J9ABC");
  expect(jobFromPath("/jobs/01J9ABC/")).toBe("01J9ABC");
  expect(jobFromPath("/")).toBeNull();
  expect(jobFromPath("/api/jobs")).toBeNull();
});
