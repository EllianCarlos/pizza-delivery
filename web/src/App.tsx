import { useMemo, useState } from "react";
import Alert from "@cloudscape-design/components/alert";
import AppLayout from "@cloudscape-design/components/app-layout";
import Button from "@cloudscape-design/components/button";
import Form from "@cloudscape-design/components/form";
import FormField from "@cloudscape-design/components/form-field";
import Header from "@cloudscape-design/components/header";
import Input from "@cloudscape-design/components/input";
import Modal from "@cloudscape-design/components/modal";
import SegmentedControl from "@cloudscape-design/components/segmented-control";
import SpaceBetween from "@cloudscape-design/components/space-between";
import Tabs from "@cloudscape-design/components/tabs";
import Textarea from "@cloudscape-design/components/textarea";
import { createApi } from "./api";
import { usePolled } from "./hooks";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobView } from "./generated/JobView";
import { jobFromPath, landingTab, pipelineLanes } from "./model";
import { enablePush, pushSupported } from "./push";
import { ArtifactBrowser, DetailPanel, EventsPanel, InboxPanel } from "./panels";
import { BoardView, PipelineView, TableView } from "./views";

const api = createApi();
type ViewName = "table" | "board" | "pipeline";

function SubmitModal({ visible, onClose, onSubmitted }: { visible: boolean; onClose: () => void; onSubmitted: () => void }) {
  const [prompt, setPrompt] = useState("");
  const [repo, setRepo] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal
      visible={visible}
      onDismiss={onClose}
      header="Submit a job"
      footer={
        <Button
          variant="primary"
          disabled={prompt.trim() === ""}
          onClick={async () => {
            try {
              await api.submit(prompt, repo || undefined);
              setPrompt("");
              setRepo("");
              setError(null);
              onSubmitted();
              onClose();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Submit
        </Button>
      }
    >
      <Form>
        <SpaceBetween size="m">
          {error && <Alert type="error">{error}</Alert>}
          <FormField label="Prompt">
            <Textarea value={prompt} onChange={({ detail }) => setPrompt(detail.value)} />
          </FormField>
          <FormField label="Repository (optional)">
            <Input value={repo} onChange={({ detail }) => setRepo(detail.value)} />
          </FormField>
        </SpaceBetween>
      </Form>
    </Modal>
  );
}

function JobsPage({ jobs, onChanged }: { jobs: JobView[]; onChanged: () => void }) {
  const [view, setView] = useState<ViewName>("table");
  const [selected, setSelected] = useState<string | null>(jobFromPath(window.location.pathname));
  const [submitting, setSubmitting] = useState(false);
  const details = usePolled<JobDetailView[]>(
    () => Promise.all(jobs.map((j) => api.job(j.short_id))),
    3000,
  );
  const lanes = useMemo(() => pipelineLanes(details.data ?? []), [details.data]);
  const detail = details.data?.find((d) => d.job.id === selected) ?? null;
  const pick = (job: JobView) => setSelected(job.id);
  const common = { selected, onSelect: pick };
  return (
    <SpaceBetween size="l">
      <Header
        variant="h1"
        actions={
          <SpaceBetween size="xs" direction="horizontal">
            <SegmentedControl
              selectedId={view}
              onChange={({ detail }) => setView(detail.selectedId as ViewName)}
              options={[
                { id: "table", text: "Table" },
                { id: "board", text: "Board" },
                { id: "pipeline", text: "Pipeline" },
              ]}
            />
            <Button variant="primary" disabled={api.staleSince() !== null} onClick={() => setSubmitting(true)}>
              Submit job
            </Button>
          </SpaceBetween>
        }
      >
        Jobs
      </Header>
      {view === "table" && <TableView jobs={jobs} {...common} />}
      {view === "board" && <BoardView jobs={jobs} {...common} />}
      {view === "pipeline" && <PipelineView lanes={lanes} {...common} />}
      {detail && <DetailPanel detail={detail} api={api} onChanged={() => { onChanged(); void details.refresh(); }} />}
      <SubmitModal visible={submitting} onClose={() => setSubmitting(false)} onSubmitted={onChanged} />
    </SpaceBetween>
  );
}

export function App() {
  const jobs = usePolled(() => api.jobs(), 3000);
  const list = jobs.data?.jobs ?? [];
  const staleAt = api.staleSince();
  const [pushError, setPushError] = useState<string | null>(null);
  const [pushOn, setPushOn] = useState(false);
  const initialTab = jobFromPath(window.location.pathname)
    ? "jobs"
    : landingTab(window.innerWidth);
  const [tab, setTab] = useState<string>(initialTab);
  return (
    <AppLayout
      navigationHide
      toolsHide
      content={
        <SpaceBetween size="m">
          {jobs.error && <Alert type="error">{jobs.error}</Alert>}
          {staleAt !== null && (
            <Alert type="warning" header="Offline">
              Showing a read-only snapshot from {new Date(staleAt).toLocaleString()}.
            </Alert>
          )}
          {pushError && <Alert type="error">{pushError}</Alert>}
          {pushSupported() && !pushOn && (
            <Button
              onClick={async () => {
                try {
                  await enablePush(api);
                  setPushOn(true);
                  setPushError(null);
                } catch (e) {
                  setPushError(e instanceof Error ? e.message : String(e));
                }
              }}
            >
              Enable notifications
            </Button>
          )}
          <Tabs
            activeTabId={tab}
            onChange={({ detail }) => setTab(detail.activeTabId)}
            tabs={[
              { id: "jobs", label: "Jobs", content: <JobsPage jobs={list} onChanged={() => void jobs.refresh()} /> },
              { id: "inbox", label: "Inbox", content: <InboxPanel api={api} /> },
              { id: "events", label: "Events", content: <EventsPanel api={api} /> },
              { id: "artifacts", label: "Artifacts", content: <ArtifactBrowser jobs={list} api={api} /> },
            ]}
          />
        </SpaceBetween>
      }
    />
  );
}
