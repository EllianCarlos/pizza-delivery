import { useEffect, useMemo, useState } from "react";
import Alert from "@cloudscape-design/components/alert";
import Box from "@cloudscape-design/components/box";
import AppLayout from "@cloudscape-design/components/app-layout";
import Button from "@cloudscape-design/components/button";
import Checkbox from "@cloudscape-design/components/checkbox";
import ExpandableSection from "@cloudscape-design/components/expandable-section";
import Form from "@cloudscape-design/components/form";
import FormField from "@cloudscape-design/components/form-field";
import Header from "@cloudscape-design/components/header";
import Input from "@cloudscape-design/components/input";
import Modal from "@cloudscape-design/components/modal";
import Select from "@cloudscape-design/components/select";
import SegmentedControl from "@cloudscape-design/components/segmented-control";
import SpaceBetween from "@cloudscape-design/components/space-between";
import Container from "@cloudscape-design/components/container";
import SideNavigation from "@cloudscape-design/components/side-navigation";
import Textarea from "@cloudscape-design/components/textarea";
import { createApi } from "./api";
import { usePolled } from "./hooks";
import type { JobDetailView } from "./generated/JobDetailView";
import type { JobView } from "./generated/JobView";
import { hashFor, jobFromPath, landingRoute, pipelineLanes, routeFromHash, STATE_ORDER, type Route } from "./model";
import { buildOptions, emptyOptionFields, type OptionFields } from "./options";
import { previewSummary, type TriagePreview } from "./triage";
import { enablePush, pushSupported } from "./push";
import { ArtifactBrowser, DetailPanel, EventsPanel, InboxPanel } from "./panels";
import { BoardView, PipelineView, TableView } from "./views";

const api = createApi();
type ViewName = "table" | "board" | "pipeline";

function PreviewResult({ preview }: { preview: TriagePreview }) {
  const s = previewSummary(preview);
  return (
    <Alert type={s.resolved ? "info" : "warning"} header="Triage preview (nothing was enqueued)">
      <SpaceBetween size="xs">
        {s.resolved ? (
          <>
            <Box><b>Repository</b> {s.repo}</Box>
            {s.alternatives.length > 0 && <Box><b>Alternatives</b> {s.alternatives.join(", ")}</Box>}
            <Box><b>Template</b> {s.template}</Box>
            <Box>
              <b>Plan</b>
              <ol style={{ margin: 0 }}>
                {s.stages.map((st) => (
                  <li key={st.n}>{st.name}: {st.reason}</li>
                ))}
              </ol>
            </Box>
            {s.risks.length > 0 && <Box><b>Risks</b> {s.risks.join(", ")}</Box>}
            <Box>{s.task}</Box>
          </>
        ) : (
          <Box>No repository resolved</Box>
        )}
        {s.notes.map((n) => <Box key={n}>{n}</Box>)}
        {s.sources.length > 0 && <Box><b>Context</b> {s.sources.join(", ")}</Box>}
      </SpaceBetween>
    </Alert>
  );
}

function SubmitModal({ visible, onClose, onSubmitted }: { visible: boolean; onClose: () => void; onSubmitted: () => void }) {
  const [prompt, setPrompt] = useState("");
  const [repo, setRepo] = useState("");
  const [fields, setFields] = useState<OptionFields>(emptyOptionFields);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<TriagePreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const set = (patch: Partial<OptionFields>) => {
    setPreview(null);
    setFields((f) => ({ ...f, ...patch }));
  };
  const templates = [
    { value: "", label: "Automatic" },
    { value: "full-feature", label: "full-feature" },
    { value: "bug-fix", label: "bug-fix" },
    { value: "investigate", label: "investigate" },
    { value: "report", label: "report" },
  ];
  return (
    <Modal
      visible={visible}
      onDismiss={onClose}
      header="Submit a job"
      footer={
        <SpaceBetween size="xs" direction="horizontal">
          <Button
            disabled={prompt.trim() === ""}
            loading={previewing}
            onClick={async () => {
              setPreviewing(true);
              try {
                setPreview(await api.preview(prompt, repo || undefined, buildOptions(fields)));
                setError(null);
              } catch (e) {
                setPreview(null);
                setError(e instanceof Error ? e.message : String(e));
              } finally {
                setPreviewing(false);
              }
            }}
          >
            Preview triage
          </Button>
          <Button
            variant="primary"
            disabled={prompt.trim() === ""}
            onClick={async () => {
              try {
                await api.submit(prompt, repo || undefined, undefined, buildOptions(fields));
                setPrompt("");
                setRepo("");
                setFields(emptyOptionFields);
                setPreview(null);
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
        </SpaceBetween>
      }
    >
      <Form>
        <SpaceBetween size="m">
          {error && <Alert type="error">{error}</Alert>}
          {preview && <PreviewResult preview={preview} />}
          <FormField label="Prompt">
            <Textarea value={prompt} onChange={({ detail }) => { setPreview(null); setPrompt(detail.value); }} />
          </FormField>
          <FormField label="Repository (optional)">
            <Input value={repo} onChange={({ detail }) => { setPreview(null); setRepo(detail.value); }} />
          </FormField>
          <ExpandableSection
            headerText="Additional Parameters"
            expanded={fields.open}
            onChange={({ detail }) => set({ open: detail.expanded })}
          >
            <SpaceBetween size="m">
              <FormField label="Template">
                <Select
                  selectedOption={templates.find((t) => t.value === fields.template) ?? templates[0]!}
                  options={templates}
                  onChange={({ detail }) => set({ template: detail.selectedOption.value as OptionFields["template"] })}
                />
              </FormField>
              <Checkbox checked={fields.reportOnly} onChange={({ detail }) => set({ reportOnly: detail.checked })}>
                Report only
              </Checkbox>
              <FormField label="Max stages" description="1 to 10">
                <Input value={fields.maxStages} inputMode="numeric" onChange={({ detail }) => set({ maxStages: detail.value })} />
              </FormField>
              <FormField label="Context hint">
                <Textarea value={fields.contextHint} onChange={({ detail }) => set({ contextHint: detail.value })} />
              </FormField>
              <FormField label="Risk flags" description="Comma-separated, kebab-case">
                <Input value={fields.riskFlags} onChange={({ detail }) => set({ riskFlags: detail.value })} />
              </FormField>
            </SpaceBetween>
          </ExpandableSection>
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

const AGENT_WORKER_LINKS: { route: Exclude<Route, "home">; text: string }[] = [
  { route: "jobs", text: "Jobs" },
  { route: "inbox", text: "Inbox" },
  { route: "events", text: "Events" },
  { route: "artifacts", text: "Artifacts" },
];

function HomePage({ jobs }: { jobs: JobView[] }) {
  const counts = STATE_ORDER.map((state) => ({ state, n: jobs.filter((j) => j.state === state).length })).filter((c) => c.n > 0);
  return (
    <SpaceBetween size="l">
      <Header variant="h1" description="Everything running on this machine.">
        Pizza delivery
      </Header>
      <Container
        header={
          <Header variant="h2" actions={<Button href={hashFor("jobs")}>Open agent workers</Button>}>
            Agent workers
          </Header>
        }
      >
        <SpaceBetween size="s">
          <div>
            {jobs.length === 0 ? "No jobs yet." : counts.map((c) => `${c.n} ${c.state}`).join(" · ")}
          </div>
          <SpaceBetween size="xs" direction="horizontal">
            {AGENT_WORKER_LINKS.map((l) => (
              <Button key={l.route} href={hashFor(l.route)}>
                {l.text}
              </Button>
            ))}
          </SpaceBetween>
        </SpaceBetween>
      </Container>
    </SpaceBetween>
  );
}

export function App() {
  const jobs = usePolled(() => api.jobs(), 3000);
  const list = jobs.data?.jobs ?? [];
  const staleAt = api.staleSince();
  const [pushError, setPushError] = useState<string | null>(null);
  const [pushOn, setPushOn] = useState(false);
  const [route, setRoute] = useState<Route>(
    () =>
      routeFromHash(window.location.hash) ??
      (jobFromPath(window.location.pathname) ? "jobs" : landingRoute(window.innerWidth)),
  );
  useEffect(() => {
    const onHash = () => setRoute(routeFromHash(window.location.hash) ?? "home");
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  return (
    <AppLayout
      toolsHide
      navigation={
        <SideNavigation
          activeHref={hashFor(route)}
          header={{ text: "Pizza delivery", href: hashFor("home") }}
          items={[
            { type: "link", text: "Home", href: hashFor("home") },
            {
              type: "section",
              text: "Agent workers",
              defaultExpanded: true,
              items: AGENT_WORKER_LINKS.map((l) => ({ type: "link" as const, text: l.text, href: hashFor(l.route) })),
            },
          ]}
        />
      }
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
          {route === "home" && <HomePage jobs={list} />}
          {route === "jobs" && <JobsPage jobs={list} onChanged={() => void jobs.refresh()} />}
          {route === "inbox" && <InboxPanel api={api} />}
          {route === "events" && <EventsPanel api={api} />}
          {route === "artifacts" && <ArtifactBrowser jobs={list} api={api} />}
        </SpaceBetween>
      }
    />
  );
}
