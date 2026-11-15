import { expect, test } from "bun:test";
import { buildOptions, emptyOptionFields, MODEL_SUGGESTIONS } from "./options";

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
      model: " haiku ",
    }),
  ).toEqual({
    template: "bug-fix",
    report_only: true,
    max_stages: 3,
    context_hint: "auth path",
    risk_flags: ["production", "secrets"],
    model: "haiku",
  });
});

test("a non-numeric count is rejected before sending", () => {
  expect(() => buildOptions({ ...emptyOptionFields, open: true, maxStages: "abc" })).toThrow("whole number");
});

test("model suggestions are unique, non-empty and accepted by the option builder", () => {
  const values = MODEL_SUGGESTIONS.map((s) => s.value);
  expect(new Set(values).size).toBe(values.length);
  for (const model of values) {
    expect(model).not.toBe("");
    expect(buildOptions({ ...emptyOptionFields, open: true, model })).toEqual({ model });
  }
});

test("model suggestions cover multiple supported harnesses with harness prefixes", () => {
  const values = MODEL_SUGGESTIONS.map((s) => s.value);
  expect(values).toContain("haiku");
  expect(values).toContain("sonnet");
  expect(values).toContain("opus");
  expect(values).toContain("gemini-flash");
  expect(values).toContain("deepseek/chat");

  const byValue = new Map(MODEL_SUGGESTIONS.map((s) => [s.value, s.description]));
  expect(byValue.get("haiku")).toStartWith("Claude:");
  expect(byValue.get("sonnet")).toStartWith("Claude:");
  expect(byValue.get("opus")).toStartWith("Claude:");
  expect(byValue.get("gemini-flash")).toStartWith("Agy:");
  expect(byValue.get("deepseek/chat")).toStartWith("Opencode:");
});
