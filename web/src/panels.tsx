import { useEffect, useMemo, useState } from "react";
import Alert from "@cloudscape-design/components/alert";
import Box from "@cloudscape-design/components/box";
import Button from "@cloudscape-design/components/button";
import Container from "@cloudscape-design/components/container";
import Header from "@cloudscape-design/components/header";
import Multiselect from "@cloudscape-design/components/multiselect";
import Input from "@cloudscape-design/components/input";
import Select from "@cloudscape-design/components/select";
import SpaceBetween from "@cloudscape-design/components/space-between";
import Table from "@cloudscape-design/components/table";
import Textarea from "@cloudscape-design/components/textarea";
import Toggle from "@cloudscape-design/components/toggle";
import type { Api } from "./api";
import { useEventStream, usePolled } from "./hooks";
import type { InboxStatus } from "./inbox";
import type { InboxEntry } from "./generated/InboxEntry";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobView } from "./generated/JobView";
import {
  artifactsOf,
  eventFamilies,
  formatCost,
  formatDuration,
  formatTokens,
  jobSummary,
  stageRows,
  toolCallsOf,
  visibleEvents,
} from "./model";
import { StateBadge } from "./views";

export function DetailPanel({
  detail,
  api,
  onChanged,
}: {
  detail: JobDetailView;
  api: Api;
  onChanged: () => void;
}) {
  const cancellable = detail.job.state === "queued" || detail.job.state === "running";
  const [showTools, setShowTools] = useState(false);
  const [now, setNow] = useState(() => Date.now());
  const anyRunning = detail.stages.some((s) => s.state === "running");
  useEffect(() => {
    if (!anyRunning) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [anyRunning]);
  const rows = useMemo(() => stageRows(detail, now), [detail, now]);
  const summary = useMemo(() => jobSummary(detail, now), [detail, now]);
  const toolCalls = useMemo(() => toolCallsOf(detail.events), [detail.events]);
  return (
    <SpaceBetween size="m">
      <Header
        variant="h2"
        description={detail.job.prompt}
        actions={
          cancellable ? (
            <Button
              onClick={async () => {
                await api.cancel(detail.job.short_id);
                onChanged();
              }}
            >
              Cancel
            </Button>
          ) : undefined
        }
      >
        {detail.job.short_id} <StateBadge state={detail.job.state} />
      </Header>
      <Box>
        <b>Total time</b> {formatDuration(summary.wallMs)}
        {" | "}
        <b>Total cost</b> {formatCost(summary.costMicrousd)}
        {" | "}
        <b>Models</b> {summary.models.length > 0 ? summary.models.join(", ") : "-"}
      </Box>
      <Table
        variant="embedded"
        header={<Header variant="h3">Stages</Header>}
        items={rows}
        trackBy="id"
        empty={<Box>No stages have run</Box>}
        columnDefinitions={[
          { id: "n", header: "#", cell: (s) => detail.stages.find((x) => x.id === s.id)?.n },
          { id: "name", header: "Stage", cell: (s) => s.name },
          { id: "state", header: "State", cell: (s) => <StateBadge state={s.state} /> },
          { id: "attempt", header: "Attempt", cell: (s) => detail.stages.find((x) => x.id === s.id)?.attempt },
          { id: "agent", header: "Agent", cell: (s) => s.agent ?? "-" },
          { id: "model", header: "Model", cell: (s) => s.model ?? "-" },
          {
            id: "time",
            header: "Time",
            cell: (s) => formatDuration(s.wallMs) + (s.live ? " (running)" : ""),
          },
          { id: "tokens", header: "Tokens", cell: (s) => formatTokens(s.inputTokens, s.outputTokens) },
          { id: "cost", header: "Cost", cell: (s) => formatCost(s.costMicrousd) },
        ]}
      />
      <Toggle checked={showTools} onChange={({ detail: d }) => setShowTools(d.checked)}>
        Show tool calls
      </Toggle>
      {showTools && (
        <Table
          variant="embedded"
          header={<Header variant="h3" counter={`(${toolCalls.length})`}>Tool calls</Header>}
          items={toolCalls}
          trackBy="cursor"
          empty={<Box>No tool calls recorded</Box>}
          columnDefinitions={[
            { id: "time", header: "Time", cell: (t) => new Date(t.ts_ms).toLocaleTimeString() },
            {
              id: "stage",
              header: "Stage",
              cell: (t) => detail.stages.find((s) => s.id === t.stage_id)?.name ?? "-",
            },
            { id: "tool", header: "Tool", cell: (t) => t.name },
            { id: "summary", header: "Summary", cell: (t) => t.summary },
          ]}
        />
      )}
      <Table
        variant="embedded"
        header={<Header variant="h3">Feedback</Header>}
        items={detail.feedback}
        trackBy="id"
        empty={<Box>No feedback</Box>}
        columnDefinitions={[
          { id: "kind", header: "Kind", cell: (e) => e.kind },
          { id: "status", header: "Status", cell: (e) => e.status },
          { id: "sev", header: "Severity", cell: (e) => e.severity },
          { id: "text", header: "Text", cell: (e) => e.text },
          { id: "evidence", header: "Evidence", cell: (e) => e.evidence ?? "-" },
        ]}
      />
      <ArtifactList detail={detail} api={api} />
    </SpaceBetween>
  );
}

export function ArtifactList({ detail, api }: { detail: JobDetailView; api: Api }) {
  const artifacts = useMemo(() => artifactsOf(detail.events), [detail.events]);
  const [open, setOpen] = useState<number | null>(null);
  const [content, setContent] = useState<string>("");
  useEffect(() => {
    setOpen(null);
    setContent("");
  }, [detail.job.id]);
  useEffect(() => {
    if (open === null) return;
    api
      .artifact(detail.job.short_id, open)
      .then((a) => setContent(a.content))
      .catch((e: Error) => setContent(e.message));
  }, [open, detail.job.short_id, api]);
  return (
    <SpaceBetween size="s">
      <Table
        variant="embedded"
        header={<Header variant="h3">Artifacts</Header>}
        items={artifacts}
        trackBy="index"
        empty={<Box>No artifacts</Box>}
        columnDefinitions={[
          { id: "kind", header: "Kind", cell: (a) => a.kind },
          { id: "path", header: "Path", cell: (a) => a.path },
          { id: "sha", header: "sha256", cell: (a) => a.sha256.slice(0, 12) },
          { id: "open", header: "", cell: (a) => <Button variant="inline-link" onClick={() => setOpen(a.index)}>View</Button> },
        ]}
      />
      {open !== null && (
        <Container header={<Header variant="h3">{artifacts[open]?.path}</Header>}>
          <pre style={{ whiteSpace: "pre-wrap", margin: 0 }}>{content}</pre>
        </Container>
      )}
    </SpaceBetween>
  );
}

const STATUS_OPTIONS: Array<{ value: InboxStatus; label: string }> = [
  { value: "active", label: "Open and answered" },
  { value: "open", label: "Open" },
  { value: "answered", label: "Answered" },
  { value: "dismissed", label: "Dismissed" },
  { value: "all", label: "All" },
];

export function EntryDetail({ entry }: { entry: InboxEntry }) {
  return (
    <Container header={<Header variant="h3">{entry.short_id}</Header>}>
      <SpaceBetween size="xs">
        <Box>{entry.text}</Box>
        <Box>
          <b>Evidence</b> {entry.evidence ?? "-"}
        </Box>
        <Box>
          <b>Confidence</b> {entry.confidence ?? "-"}
          {" | "}
          <b>Impact</b> {entry.impact ?? "-"}
          {" | "}
          <b>Alternatives</b> {entry.alternatives.length > 0 ? entry.alternatives.join(", ") : "-"}
        </Box>
      </SpaceBetween>
    </Container>
  );
}

export function InboxPanel({ api }: { api: Api }) {
  const [status, setStatus] = useState<InboxStatus>("active");
  const [query, setQuery] = useState("");
  const { data, error, refresh } = usePolled(() => api.inbox({ status, q: query }), 5000, [status, query]);
  const [selected, setSelected] = useState<InboxEntry[]>([]);
  const [text, setText] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const entries = data?.entries ?? [];
  // The server only accepts open entries; one answered entry rejects the whole batch.
  const open = selected.filter((e) => e.status === "open");
  return (
    <SpaceBetween size="m">
      {error && <Alert type="error">{error}</Alert>}
      {actionError && <Alert type="error" dismissible onDismiss={() => setActionError(null)}>{actionError}</Alert>}
      {note && <Alert type="success" dismissible onDismiss={() => setNote(null)}>{note}</Alert>}
      <SpaceBetween size="xs" direction="horizontal">
        <Select
          selectedOption={STATUS_OPTIONS.find((o) => o.value === status) ?? STATUS_OPTIONS[0]!}
          options={STATUS_OPTIONS}
          onChange={({ detail }) => setStatus((detail.selectedOption.value ?? "active") as InboxStatus)}
          ariaLabel="Filter by status"
        />
        <Input
          type="search"
          value={query}
          onChange={({ detail }) => setQuery(detail.value)}
          placeholder="Search kind, severity or text"
          ariaLabel="Search the inbox"
        />
      </SpaceBetween>
      <Table
        header={<Header counter={`(${entries.length})`}>Feedback inbox</Header>}
        items={entries}
        trackBy="id"
        selectionType="multi"
        selectedItems={selected}
        onSelectionChange={({ detail }) => setSelected(detail.selectedItems)}
        empty={<Box textAlign="center">Nothing waiting for you</Box>}
        columnDefinitions={[
          { id: "id", header: "Entry", cell: (e) => e.short_id },
          { id: "kind", header: "Kind", cell: (e) => e.kind },
          { id: "sev", header: "Severity", cell: (e) => e.severity },
          { id: "text", header: "Text", cell: (e) => e.text },
          { id: "conf", header: "Confidence", cell: (e) => e.confidence ?? "-" },
          { id: "status", header: "Status", cell: (e) => e.status },
        ]}
      />
      {selected.length === 1 && selected[0] && <EntryDetail entry={selected[0]} />}
      <Textarea value={text} onChange={({ detail }) => setText(detail.value)} placeholder="Reply to the selected entries" />
      <SpaceBetween size="xs" direction="horizontal">
        <Button
          variant="primary"
          disabled={open.length === 0 || text.trim() === ""}
          onClick={async () => {
            try {
              const out = await api.reply(open.map((e) => e.id), text);
              setNote(`Follow-up job ${out.short_id} created`);
              setActionError(null);
              setSelected([]);
              setText("");
              await refresh();
            } catch (e) {
              setActionError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Reply
        </Button>
        <Button
          disabled={open.length === 0}
          onClick={async () => {
            try {
              await api.dismiss(open.map((e) => e.id));
              setNote(`Dismissed ${open.length} ${open.length === 1 ? "entry" : "entries"}`);
              setActionError(null);
              setSelected([]);
              await refresh();
            } catch (e) {
              setActionError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Dismiss
        </Button>
      </SpaceBetween>
    </SpaceBetween>
  );
}

export function EventsPanel({ api }: { api: Api }) {
  const { data } = usePolled(() => api.events(0), 60000);
  const events = useEventStream(data?.events ?? []);
  const [families, setFamilies] = useState<string[]>([]);
  const options = eventFamilies(events).map((f) => ({ label: f, value: f }));
  const [showTools, setShowTools] = useState(false);
  const shown = visibleEvents(events, new Set(families), showTools);
  return (
    <SpaceBetween size="m">
      <Toggle checked={showTools} onChange={({ detail }) => setShowTools(detail.checked)}>
        Show tool calls
      </Toggle>
      <Multiselect
        selectedOptions={families.map((f) => ({ label: f, value: f }))}
        options={options}
        onChange={({ detail }) => setFamilies(detail.selectedOptions.map((o) => o.value ?? ""))}
        placeholder="Filter by event family"
      />
      <Table
        header={<Header counter={`(${shown.length})`}>Event stream</Header>}
        items={[...shown].reverse()}
        trackBy="cursor"
        columnDefinitions={[
          { id: "cursor", header: "#", cell: (e) => e.cursor },
          { id: "time", header: "Time", cell: (e) => new Date(e.ts_ms).toLocaleTimeString() },
          { id: "job", header: "Job", cell: (e) => e.job_id.slice(-6) },
          { id: "actor", header: "Actor", cell: (e) => e.actor },
          { id: "kind", header: "Kind", cell: (e) => e.kind },
        ]}
      />
    </SpaceBetween>
  );
}

export function ArtifactBrowser({ jobs, api }: { jobs: JobView[]; api: Api }) {
  const [ref, setRef] = useState<string | null>(null);
  const { data } = usePolled(() => (ref ? api.job(ref) : Promise.resolve(null)), 5000);
  return (
    <SpaceBetween size="m">
      <Select
        selectedOption={ref ? { label: ref, value: ref } : null}
        options={jobs.map((j) => ({ label: `${j.short_id}  ${j.prompt.slice(0, 60)}`, value: j.short_id }))}
        onChange={({ detail }) => setRef(detail.selectedOption.value ?? null)}
        placeholder="Choose a job"
      />
      {data && <ArtifactList detail={data} api={api} />}
    </SpaceBetween>
  );
}
