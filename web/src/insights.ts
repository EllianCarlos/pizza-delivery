/** Shapes of `GET /api/insights` and `GET /api/suppressions` (see insights.rs and suppress.rs in the core crate). */
export type CausePattern = {
  event: string;
  stage: string;
  agent: string;
  cause: string;
  count: number;
  jobs: string[];
};

export type JobFeedback = { job: string; recorded: number; dismissed: number; no_issue: number };

export type FeedbackRates = {
  recorded: number;
  dismissed: number;
  no_issue: number;
  dismissed_permille: number;
  no_issue_permille: number;
};

export type StageStat = {
  stage: string;
  agent: string;
  runs: number;
  total_ms: number;
  avg_ms: number;
  max_ms: number;
  tokens: number;
  cost_microusd: number;
};

export type LeaseExpiries = { holder: string; count: number; jobs: string[] };

export type InsightsView = {
  causes: CausePattern[];
  feedback_by_job: JobFeedback[];
  feedback: FeedbackRates;
  stages: StageStat[];
  lease_expiries: LeaseExpiries[];
};

export type SuppressionRule = {
  id: string;
  repo: string;
  kind: string;
  text: string;
  reason: string;
  entry: string;
  job: string;
};

export function percent(permille: number): string {
  return `${(permille / 10).toFixed(1)}%`;
}

export function seconds(ms: number): string {
  return `${(ms / 1000).toFixed(1)}s`;
}

export function dollars(microusd: number): string {
  return `$${(microusd / 1_000_000).toFixed(4)}`;
}

/** "review: invalid-handoff x3", the same label the CLI prints. */
export function causeLabel(c: CausePattern): string {
  return `${c.stage}: ${c.cause} x${c.count}`;
}
