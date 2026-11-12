// Pure mapping from form fields to the JSON body of POST /api/submit and
// POST /api/dry-run. The server stays the authority on validation.

/**
 * @param {{prompt?: string, repo?: string, priority?: string, expanded?: boolean,
 *   template?: string, reportOnly?: boolean, maxStages?: string|number,
 *   contextHint?: string, riskFlags?: string}} fields
 */
export function buildSubmitBody(fields) {
  const body = { prompt: fields.prompt ?? "" };
  const repo = (fields.repo ?? "").trim();
  if (repo) body.repo = repo;
  if (fields.priority) body.priority = fields.priority;

  const options = {};
  if (fields.expanded) {
    if (fields.template) options.template = fields.template;
    if (fields.reportOnly) options.report_only = true;
    const stages = String(fields.maxStages ?? "").trim();
    if (stages) options.max_stages = /^-?\d+$/.test(stages) ? Number(stages) : stages;
    const hint = (fields.contextHint ?? "").trim();
    if (hint) options.context_hint = hint;
    const flags = (fields.riskFlags ?? "").split(/[\s,]+/).filter(Boolean);
    if (flags.length) options.risk_flags = flags;
  }
  if (Object.keys(options).length) body.options = options;
  return body;
}

function readFields(form) {
  const value = (name) => form.elements[name]?.value ?? "";
  return {
    prompt: value("prompt"),
    repo: value("repo"),
    priority: value("priority"),
    expanded: form.querySelector("details")?.open ?? false,
    template: value("template"),
    reportOnly: form.elements["report_only"]?.checked ?? false,
    maxStages: value("max_stages"),
    contextHint: value("context_hint"),
    riskFlags: value("risk_flags"),
  };
}

async function post(path, body, out) {
  out.className = "";
  out.textContent = "Working...";
  try {
    const res = await fetch(path, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(body),
    });
    const data = await res.json().catch(() => null);
    if (!res.ok) {
      out.className = "error";
      out.textContent = data?.error?.message ?? `HTTP ${res.status}`;
      return;
    }
    out.textContent =
      data.type === "submitted"
        ? `Submitted job ${data.short_id}`
        : JSON.stringify(data, null, 2);
  } catch (e) {
    out.className = "error";
    out.textContent = `Request failed: ${e}`;
  }
}

if (typeof document !== "undefined") {
  const form = document.getElementById("job-form");
  const out = document.getElementById("result");
  form.addEventListener("submit", (e) => {
    e.preventDefault();
    post("/api/submit", buildSubmitBody(readFields(form)), out);
  });
  document.getElementById("preview").addEventListener("click", () => {
    const body = buildSubmitBody(readFields(form));
    delete body.priority;
    post("/api/dry-run", body, out);
  });
}
