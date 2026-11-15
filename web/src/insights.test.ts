import { expect, test } from "bun:test";
import { causeLabel, dollars, percent, seconds } from "./insights";

test("a cause reads like the CLI line", () => {
  const c = { event: "stage-retried", stage: "review", agent: "claude", cause: "invalid-handoff", count: 3, jobs: [] };
  expect(causeLabel(c)).toBe("review: invalid-handoff x3");
});

test("units are formatted for people", () => {
  expect(percent(250)).toBe("25.0%");
  expect(seconds(1500)).toBe("1.5s");
  expect(dollars(2000)).toBe("$0.0020");
});
