import { expect, test } from "bun:test";
import { buildOptions, emptyOptionFields } from "./options";

test("collapsed or empty groups send no options", () => {
  expect(buildOptions(emptyOptionFields)).toBeUndefined();
  expect(buildOptions({ ...emptyOptionFields, template: "report" })).toBeUndefined();
  expect(buildOptions({ ...emptyOptionFields, open: true })).toBeUndefined();
});

test("filled fields become typed, trimmed options", () => {
  expect(
    buildOptions({
      open: true,
      template: "bug-fix",
      reportOnly: true,
      maxStages: " 3 ",
      contextHint: "  auth path ",
      riskFlags: "production, secrets,, ",
    }),
  ).toEqual({
    template: "bug-fix",
    report_only: true,
    max_stages: 3,
    context_hint: "auth path",
    risk_flags: ["production", "secrets"],
  });
});

test("a non-numeric count is rejected before sending", () => {
  expect(() => buildOptions({ ...emptyOptionFields, open: true, maxStages: "abc" })).toThrow("whole number");
});
