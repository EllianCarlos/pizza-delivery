import Alert from "@cloudscape-design/components/alert";
import Box from "@cloudscape-design/components/box";
import Header from "@cloudscape-design/components/header";
import SpaceBetween from "@cloudscape-design/components/space-between";
import Table from "@cloudscape-design/components/table";
import type { Api } from "./api";
import { usePolled } from "./hooks";
import { causeLabel, dollars, percent, seconds } from "./insights";

const NONE = <Box color="text-status-inactive">None</Box>;

export function InsightsPanel({ api }: { api: Api }) {
  const insights = usePolled(() => api.insights(), 10000);
  const rules = usePolled(() => api.suppressions(), 10000);
  const data = insights.data;
  if (!data) {
    return insights.error ? <Alert type="error">{insights.error}</Alert> : <Box>Loading insights...</Box>;
  }
  const f = data.feedback;
  return (
    <SpaceBetween size="m">
      <Table
        header={<Header counter={`(${data.causes.length})`}>Retries and failures</Header>}
        items={data.causes}
        trackBy={(c) => `${c.event}/${c.stage}/${c.agent}/${c.cause}`}
        empty={NONE}
        columnDefinitions={[
          { id: "pattern", header: "Pattern", cell: causeLabel },
          { id: "event", header: "Event", cell: (c) => c.event },
          { id: "agent", header: "Agent", cell: (c) => c.agent },
          { id: "jobs", header: "Jobs", cell: (c) => c.jobs.map((j) => j.slice(-6)).join(", ") },
        ]}
      />
      <Table
        header={
          <Header
            description={`${f.recorded} recorded, ${f.dismissed} dismissed (${percent(f.dismissed_permille)}), ${f.no_issue} answered 'no issue' (${percent(f.no_issue_permille)})`}
          >
            Feedback
          </Header>
        }
        items={data.feedback_by_job}
        trackBy="job"
        empty={NONE}
        columnDefinitions={[
          { id: "job", header: "Job", cell: (j) => j.job.slice(-6) },
          { id: "recorded", header: "Recorded", cell: (j) => j.recorded },
          { id: "dismissed", header: "Dismissed", cell: (j) => j.dismissed },
          { id: "no-issue", header: "No issue", cell: (j) => j.no_issue },
        ]}
      />
      <Table
        header={<Header>Stage duration and cost</Header>}
        items={data.stages}
        trackBy={(s) => `${s.stage}/${s.agent}`}
        empty={NONE}
        columnDefinitions={[
          { id: "stage", header: "Stage", cell: (s) => s.stage },
          { id: "agent", header: "Agent", cell: (s) => s.agent },
          { id: "runs", header: "Runs", cell: (s) => s.runs },
          { id: "avg", header: "Average", cell: (s) => seconds(s.avg_ms) },
          { id: "max", header: "Longest", cell: (s) => seconds(s.max_ms) },
          { id: "tokens", header: "Tokens", cell: (s) => s.tokens },
          { id: "cost", header: "Cost", cell: (s) => dollars(s.cost_microusd) },
        ]}
      />
      <Table
        header={<Header>Lease expiries per daemon run</Header>}
        items={data.lease_expiries}
        trackBy="holder"
        empty={NONE}
        columnDefinitions={[
          { id: "holder", header: "Daemon", cell: (l) => l.holder },
          { id: "count", header: "Expiries", cell: (l) => l.count },
        ]}
      />
      <Table
        header={
          <Header description="Remove one with `pizza-delivery suppressions remove <id>`.">
            Suppression rules
          </Header>
        }
        items={rules.data ?? []}
        trackBy="id"
        empty={NONE}
        columnDefinitions={[
          { id: "id", header: "Rule", cell: (r) => r.id },
          { id: "reason", header: "Learned from", cell: (r) => r.reason },
          { id: "repo", header: "Repo", cell: (r) => r.repo },
          { id: "kind", header: "Kind", cell: (r) => r.kind },
          { id: "text", header: "Text", cell: (r) => r.text },
        ]}
      />
    </SpaceBetween>
  );
}
