import { useEffect, useMemo, useState } from "react";
import Alert from "@cloudscape-design/components/alert";
import Box from "@cloudscape-design/components/box";
import Button from "@cloudscape-design/components/button";
import Container from "@cloudscape-design/components/container";
import Header from "@cloudscape-design/components/header";
import Multiselect from "@cloudscape-design/components/multiselect";
import Select from "@cloudscape-design/components/select";
import SpaceBetween from "@cloudscape-design/components/space-between";
import Table from "@cloudscape-design/components/table";
import Textarea from "@cloudscape-design/components/textarea";
import type { Api } from "./api";
import { useEventStream, usePolled } from "./hooks";
import type { InboxEntry } from "./generated/InboxEntry";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobView } from "./generated/JobView";
import { artifactsOf, eventFamilies, filterEvents } from "./model";
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
      <Table
        variant="embedded"
        header={<Header variant="h3">Stages</Header>}
        items={detail.stages}
        trackBy="id"
        empty={<Box>No stages have run</Box>}
        columnDefinitions={[
          { id: "n", header: "#", cell: (s) => s.n },
          { id: "name", header: "Stage", cell: (s) => s.name },
          { id: "state", header: "State", cell: (s) => <StateBadge state={s.state} /> },
          { id: "attempt", header: "Attempt", cell: (s) => s.attempt },
          { id: "agent", header: "Agent", cell: (s) => s.usage?.agent ?? "-" },
        ]}
      />
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
          { id: "status", header: "Status", cell: (e) => e.status },
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

export function InboxPanel({ api }: { api: Api }) {
  const { data, error, refresh } = usePolled(() => api.inbox(false));
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
        ]}
      />
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
  const shown = filterEvents(events, new Set(families));
  return (
    <SpaceBetween size="m">
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
