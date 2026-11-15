import Box from "@cloudscape-design/components/box";
import ColumnLayout from "@cloudscape-design/components/column-layout";
import Container from "@cloudscape-design/components/container";
import Header from "@cloudscape-design/components/header";
import SpaceBetween from "@cloudscape-design/components/space-between";
import StatusIndicator, { type StatusIndicatorProps } from "@cloudscape-design/components/status-indicator";
import Link from "@cloudscape-design/components/link";
import Table from "@cloudscape-design/components/table";
import type { JobState } from "./generated/JobState";
import type { JobView } from "./generated/JobView";
import { groupByState, type Lane, type Segment } from "./model";

export function stateType(state: JobState | Segment["state"]): StatusIndicatorProps.Type {
  switch (state) {
    case "completed":
    case "succeeded":
      return "success";
    case "running":
      return "in-progress";
    case "failed":
      return "error";
    case "needs-human":
    case "waiting-for-quota":
      return "warning";
    case "queued":
    case "pending":
      return "pending";
    default:
      return "stopped";
  }
}

export function StateBadge({ state }: { state: JobState | Segment["state"] }) {
  return <StatusIndicator type={stateType(state)}>{state}</StatusIndicator>;
}

const emptyText = (total: number) => (total > 0 ? "No jobs match the current filters" : "No jobs yet");

type Pick = { selected: string | null; onSelect: (job: JobView) => void };

export function TableView({
  jobs,
  total = jobs.length,
  selected,
  onSelect,
  prLinks = {},
}: { jobs: JobView[]; total?: number; prLinks?: Record<string, string | null> } & Pick) {
  return (
    <Table
      variant="embedded"
      items={jobs}
      trackBy="id"
      selectionType="single"
      selectedItems={jobs.filter((j) => j.id === selected)}
      onSelectionChange={({ detail }) => {
        const job = detail.selectedItems[0];
        if (job) onSelect(job);
      }}
      empty={<Box textAlign="center">{emptyText(total)}</Box>}
      columnDefinitions={[
        { id: "id", header: "Job", cell: (j) => j.short_id },
        { id: "state", header: "State", cell: (j) => <StateBadge state={j.state} /> },
        { id: "prompt", header: "Prompt", cell: (j) => j.prompt },
        {
          id: "pr",
          header: "PR",
          cell: (j) => {
            const url = prLinks[j.id];
            return url ? (
              <Link href={url} external>
                PR
              </Link>
            ) : (
              "-"
            );
          },
        },
        { id: "at", header: "Submitted", cell: (j) => new Date(j.submitted_at_ms).toLocaleString() },
      ]}
    />
  );
}

export function BoardView({ jobs, total = jobs.length, selected, onSelect }: { jobs: JobView[]; total?: number } & Pick) {
  const columns = groupByState(jobs).filter((c) => c.jobs.length > 0);
  if (columns.length === 0) return <Box textAlign="center">{emptyText(total)}</Box>;
  return (
    <ColumnLayout columns={Math.min(columns.length, 4)}>
      {columns.map((column) => (
        <SpaceBetween key={column.state} size="s">
          <Header variant="h3" counter={`(${column.jobs.length})`}>
            {column.state}
          </Header>
          {column.jobs.map((job) => (
            <div
              key={job.id}
              role="button"
              tabIndex={0}
              onClick={() => onSelect(job)}
              onKeyDown={(e) => e.key === "Enter" && onSelect(job)}
              style={{ cursor: "pointer" }}
            >
              <Container header={<Header variant="h3">{job.short_id}</Header>}>
                <Box color={job.id === selected ? "text-status-info" : undefined}>{job.prompt}</Box>
              </Container>
            </div>
          ))}
        </SpaceBetween>
      ))}
    </ColumnLayout>
  );
}

export function PipelineView({ lanes, total = lanes.length, selected, onSelect }: { lanes: Lane[]; total?: number } & Pick) {
  if (lanes.length === 0) return <Box textAlign="center">{emptyText(total)}</Box>;
  return (
    <SpaceBetween size="m">
      {lanes.map((lane) => (
        <div
          key={lane.job.id}
          role="button"
          tabIndex={0}
          onClick={() => onSelect(lane.job)}
          onKeyDown={(e) => e.key === "Enter" && onSelect(lane.job)}
          style={{ cursor: "pointer" }}
        >
          <SpaceBetween size="xs">
            <Box fontWeight={lane.job.id === selected ? "bold" : "normal"}>
              {lane.job.short_id} <StateBadge state={lane.job.state} /> {lane.job.prompt}
            </Box>
            <div style={{ display: "flex", gap: 4 }}>
              {lane.segments.map((segment, i) => (
                <div key={`${segment.name}-${i}`} style={{ flex: 1, padding: "4px 8px", border: "1px solid #888" }}>
                  <StatusIndicator type={stateType(segment.state)}>{segment.name}</StatusIndicator>
                </div>
              ))}
            </div>
          </SpaceBetween>
        </div>
      ))}
    </SpaceBetween>
  );
}
