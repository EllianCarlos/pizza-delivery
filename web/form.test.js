import { expect, test } from "bun:test";
import { buildSubmitBody } from "./form.js";

test("collapsed expander omits options even when fields are filled", () => {
  expect(buildSubmitBody({ prompt: "x", expanded: false, template: "bug-fix", maxStages: "3" })).toEqual({ prompt: "x" });
});

test("expanded but empty omits options", () => {
  expect(buildSubmitBody({ prompt: "x", expanded: true, template: "", reportOnly: false, maxStages: " ", contextHint: "  ", riskFlags: " , " })).toEqual({ prompt: "x" });
});

test("filled fields emit only those fields with correct types", () => {
  const body = buildSubmitBody({
    prompt: "x", repo: " /r ", priority: "low", expanded: true,
    template: "investigate", reportOnly: true, maxStages: "4",
    contextHint: "  see the ADR  ", riskFlags: "touches-auth, schema-change  data-loss",
  });
  expect(body).toEqual({
    prompt: "x", repo: "/r", priority: "low",
    options: { template: "investigate", report_only: true, max_stages: 4, context_hint: "see the ADR", risk_flags: ["touches-auth", "schema-change", "data-loss"] },
  });
});

test("only the set option is sent", () => {
  expect(buildSubmitBody({ prompt: "x", expanded: true, maxStages: 2 }).options).toEqual({ max_stages: 2 });
});

test("a non-numeric max stages is passed through for the server to reject", () => {
  expect(buildSubmitBody({ prompt: "x", expanded: true, maxStages: "abc" }).options).toEqual({ max_stages: "abc" });
});
