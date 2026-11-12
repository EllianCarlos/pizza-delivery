/** Wire shape of `POST /api/dry-run` (core `Outcome` plus the context sources). */
export interface TriagePreview {
  outcome: {
    triage: {
      repo: { path: string; confidence: number; alternatives: Array<{ name: string; path: string; score: number }> };
      template: string;
      plan: Array<{ name: string; reason: string }>;
      risk_flags: string[];
      task: string;
    } | null;
    feedback: Array<{ kind: string; severity: string; text: string; confidence: number | null }>;
  };
  context_sources: Array<{ source: string; bytes: number; truncated: boolean }>;
}

export interface PreviewSummary {
  resolved: boolean;
  repo: string;
  template: string;
  task: string;
  stages: Array<{ n: number; name: string; reason: string }>;
  risks: string[];
  alternatives: string[];
  notes: string[];
  sources: string[];
}

export function previewSummary(p: TriagePreview): PreviewSummary {
  const t = p.outcome.triage;
  return {
    resolved: t !== null,
    repo: t ? `${t.repo.path} (confidence ${t.repo.confidence}%)` : "",
    template: t?.template ?? "",
    task: t?.task ?? "",
    stages: (t?.plan ?? []).map((s, i) => ({ n: i + 1, name: s.name, reason: s.reason })),
    risks: t?.risk_flags ?? [],
    alternatives: (t?.repo.alternatives ?? []).map((a) => `${a.path} (${a.score}%)`),
    notes: p.outcome.feedback.map((f) => `${f.kind} (${f.severity}): ${f.text}`),
    sources: p.context_sources.map((s) => `${s.source} (${s.bytes} bytes${s.truncated ? ", truncated" : ""})`),
  };
}
