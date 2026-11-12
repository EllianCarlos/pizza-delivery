/** Wire shape of the optional "Additional Parameters" (core `TriageOptions`). */
export type TemplateName = "full-feature" | "bug-fix" | "investigate" | "report";

export interface TriageOptions {
  template?: TemplateName;
  report_only?: boolean;
  max_stages?: number;
  context_hint?: string;
  risk_flags?: string[];
  model?: string;
}

export interface OptionFields {
  open: boolean;
  template: "" | TemplateName;
  reportOnly: boolean;
  maxStages: string;
  contextHint: string;
  riskFlags: string;
  model: string;
}

export const emptyOptionFields: OptionFields = {
  open: false,
  template: "",
  reportOnly: false,
  maxStages: "",
  contextHint: "",
  riskFlags: "",
  model: "",
};

/**
 * Form fields to the `options` request key. Undefined when the group is
 * collapsed or every field is empty, so the request is identical to one
 * without options. Range and format checks stay with the server; only a
 * non-integer count is rejected here because it cannot be sent as a number.
 */
export function buildOptions(f: OptionFields): TriageOptions | undefined {
  if (!f.open) return undefined;
  const options: TriageOptions = {};
  if (f.template) options.template = f.template;
  if (f.reportOnly) options.report_only = true;
  const max = f.maxStages.trim();
  if (max !== "") {
    if (!/^\d+$/.test(max)) throw new Error("Max stages must be a whole number");
    options.max_stages = Number(max);
  }
  const hint = f.contextHint.trim();
  if (hint !== "") options.context_hint = hint;
  const flags = f.riskFlags
    .split(",")
    .map((x) => x.trim())
    .filter((x) => x !== "");
  if (flags.length > 0) options.risk_flags = flags;
  const model = f.model.trim();
  if (model !== "") options.model = model;
  return Object.keys(options).length > 0 ? options : undefined;
}
