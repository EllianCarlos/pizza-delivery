use std::collections::VecDeque;
use std::sync::atomic::AtomicI64;
use std::sync::Arc;

use pizza_delivery_core::budget::DEFAULT_CAP_TOKENS;
use pizza_delivery_core::event::{Event, FollowUp};
use pizza_delivery_core::gate::GateThresholds;
use pizza_delivery_core::job::JobState;
use pizza_delivery_core::{NewJob, Priority};

use super::*;
use pizza_delivery_core::routing::AdapterKind;

use crate::agent::{Agent, AgentOutput};
use crate::sqlite_store::SqliteStore;

const HANDOFF: &str = "---\njob: J1\nstage: implement\n---\n\n## Goal\ng\n\n## What was done\nd\n\n## Decisions made\nd\n\n## Assumptions\na\n\n## Open questions\nq\n\n## Files touched\nf\n\n## Instructions for the next stage\ni\n";
const HANDOFF_SHA256: &str = "6a202cd12dc66cd181fc76478ff436b04e9c7fa87e56577997b403d4c661e54f";
const OK_RESULT: &str = "{\"type\":\"result\",\"is_error\":false}\n";
const ERR_RESULT: &str =
    "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":404}\n";

struct FakeEnv {
    now: AtomicI64,
    next: AtomicI64,
}

impl Env for FakeEnv {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
    fn new_ulid(&self) -> String {
        format!("U{}", self.next.fetch_add(1, Ordering::SeqCst))
    }
}

#[derive(Default)]
struct FakeGit {
    dirty: AtomicBool,
    pushed: AtomicBool,
    wip: Mutex<Vec<String>>,
    removed: Mutex<Vec<PathBuf>>,
    files: Mutex<Vec<String>>,
    pushes: Mutex<Vec<(String, String)>>,
    prs: Mutex<Vec<(String, String, Vec<String>)>>,
    push_fails: AtomicBool,
    blocked: Mutex<Option<String>>,
    readiness_fails: AtomicBool,
    merge_fails: AtomicBool,
    ready_checks: Mutex<Vec<String>>,
    merges: Mutex<Vec<String>>,
}

impl Git for FakeGit {
    fn merge_readiness(&self, _worktree: &Path, pr_url: &str) -> Result<MergeReadiness> {
        acquire(&self.ready_checks, "ready_checks")?.push(pr_url.to_owned());
        if self.readiness_fails.load(Ordering::SeqCst) {
            return Err(anyhow!("gh unavailable"));
        }
        Ok(match acquire(&self.blocked, "blocked")?.clone() {
            Some(reason) => MergeReadiness::Blocked(reason),
            None => MergeReadiness::Ready,
        })
    }
    fn merge_pr(&self, _worktree: &Path, pr_url: &str) -> Result<()> {
        if self.merge_fails.load(Ordering::SeqCst) {
            return Err(anyhow!("merge rejected"));
        }
        acquire(&self.merges, "merges")?.push(pr_url.to_owned());
        Ok(())
    }
    fn changed_files(&self, _repo: &Path, _worktree: &Path) -> Result<Vec<String>> {
        Ok(acquire(&self.files, "files")?.clone())
    }
    fn push(&self, _worktree: &Path, remote: &str, branch: &str) -> Result<String> {
        if self.push_fails.load(Ordering::SeqCst) {
            return Err(anyhow!("remote rejected"));
        }
        acquire(&self.pushes, "pushes")?.push((remote.to_owned(), branch.to_owned()));
        Ok(format!("{remote}/{branch}"))
    }
    fn open_draft_pr(
        &self,
        _worktree: &Path,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<String> {
        acquire(&self.prs, "prs")?.push((
            title.to_owned(),
            body.to_owned(),
            labels.iter().map(|l| (*l).to_owned()).collect(),
        ));
        Ok("https://github.com/o/r/pull/7".to_owned())
    }
    fn ensure_worktree(
        &self,
        _repo: &Path,
        worktree: &Path,
        _branch: &str,
        _base: Option<&str>,
    ) -> Result<()> {
        Ok(std::fs::create_dir_all(worktree)?)
    }
    fn commit_wip(&self, _worktree: &Path, message: &str) -> Result<Option<String>> {
        acquire(&self.wip, "wip")?.push(message.to_owned());
        Ok(Some("wip1".to_owned()))
    }
    fn is_dirty(&self, _worktree: &Path) -> Result<bool> {
        Ok(self.dirty.load(Ordering::SeqCst))
    }
    fn is_pushed(&self, _worktree: &Path) -> Result<bool> {
        Ok(self.pushed.load(Ordering::SeqCst))
    }
    fn remove_worktree(&self, _repo: &Path, worktree: &Path) -> Result<()> {
        acquire(&self.removed, "removed")?.push(worktree.to_owned());
        Ok(())
    }
}

#[derive(Default)]
struct Script {
    handoff: Option<&'static str>,
    review: Option<&'static str>,
    stdout: &'static str,
    stderr: &'static str,
    exit_code: i32,
    timed_out: bool,
}

struct FakeAgent {
    tag: String,
    scripts: Mutex<VecDeque<Script>>,
    egress: Arc<Mutex<Option<&'static str>>>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Agent for FakeAgent {
    fn run(&self, inv: &Invocation, _on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput> {
        let requests = *acquire(&self.egress, "egress")?;
        if let Some(requests) = requests {
            std::fs::write(&inv.egress_log, requests)?;
        }
        let script = acquire(&self.scripts, "scripts")?
            .pop_front()
            .ok_or_else(|| anyhow!("no scripted run left"))?;
        acquire(&self.prompts, "prompts")?.push(inv.prompt.clone());
        std::fs::write(
            inv.handoff_path
                .with_file_name(format!("prompt-{}.txt", self.tag)),
            &inv.prompt,
        )?;
        if let Some(text) = script.review {
            std::fs::write(inv.handoff_path.with_file_name("review.md"), text)?;
        }
        if let Some(text) = script.handoff {
            std::fs::write(&inv.handoff_path, text)?;
        }
        Ok(AgentOutput {
            stdout: script.stdout.to_owned(),
            stderr: script.stderr.to_owned(),
            exit_code: Some(script.exit_code),
            timed_out: script.timed_out,
        })
    }
}

struct Rig {
    _dir: tempfile::TempDir,
    store: Mutex<Box<dyn Store>>,
    env: FakeEnv,
    git: FakeGit,
    fleet: Fleet,
    routing: RoutingConfig,
    config: WorkerConfig,
    egress: Arc<Mutex<Option<&'static str>>>,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    fn new(scripts: Vec<Script>) -> Result<Self> {
        Self::with_chain(vec![("claude", AdapterKind::Claude, scripts)], "")
    }

    fn with_chain(agents: Vec<(&str, AdapterKind, Vec<Script>)>, owner: &str) -> Result<Self> {
        Self::build(agents, owner, None)
    }

    fn build(
        agents: Vec<(&str, AdapterKind, Vec<Script>)>,
        owner: &str,
        follow_up: Option<FollowUp>,
    ) -> Result<Self> {
        let egress = Arc::new(Mutex::new(None));
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let dir = tempfile::TempDir::new()?;
        let mut store = SqliteStore::open(&dir.path().join("l.db"))?;
        store.enqueue_job(NewJob {
            id: JobId::new("J1"),
            event_id: "E0".into(),
            prompt: "do it".into(),
            repo: Some("/src/my repo".into()),
            priority: Priority::Normal,
            follow_up,
            options: None,
            submitted_at_ms: 0,
        })?;
        Ok(Self {
            config: WorkerConfig {
                data_dir: dir.path().to_owned(),
                holder: "w1".into(),
                lease_ttl_ms: 1000,
                heartbeat_ms: 100,
                timeout: Some(Duration::from_secs(5)),
                kill_grace: Duration::from_millis(50),
                caps: Caps::default(),
                retention_ms: 1000,
                budget_cap_tokens: DEFAULT_CAP_TOKENS,
                budget_cap_microusd: None,
                budget_defaults: Defaults::standard(),
                gate: GateThresholds::default(),
                require_triage: false,
                mcp: None,
                memory: None,
                auto_merge: true,
                shadow: None,
                trust: TrustConfig {
                    default_hosts: vec!["api.anthropic.com".to_owned()],
                    ..TrustConfig::default()
                },
            },
            _dir: dir,
            store: Mutex::new(Box::new(store)),
            env: FakeEnv {
                now: AtomicI64::new(100),
                next: AtomicI64::new(1),
            },
            git: FakeGit::default(),
            egress: Arc::clone(&egress),
            prompts: Arc::clone(&prompts),
            fleet: Fleet::new(
                agents
                    .into_iter()
                    .map(|(name, kind, scripts)| {
                        (
                            name.to_owned(),
                            kind,
                            Box::new(FakeAgent {
                                tag: name.to_owned(),
                                scripts: Mutex::new(scripts.into()),
                                egress: Arc::clone(&egress),
                                prompts: Arc::clone(&prompts),
                            }) as Box<dyn Agent>,
                        )
                    })
                    .collect(),
            ),
            routing: RoutingConfig::builtin("claude").with_owner(owner)?,
        })
    }

    fn with_egress(self, requests: &'static str) -> Result<Self> {
        *acquire(&self.egress, "egress")? = Some(requests);
        Ok(self)
    }

    fn audits(&self) -> Result<Vec<EventPayload>> {
        Ok(self
            .timeline()?
            .into_iter()
            .map(|e| e.payload)
            .filter(|p| matches!(p, EventPayload::SandboxAudit { .. }))
            .collect())
    }

    fn worker(&self) -> Worker<'_> {
        Worker {
            store: &self.store,
            env: &self.env,
            git: &self.git,
            fleet: &self.fleet,
            routing: &self.routing,
            config: &self.config,
        }
    }

    fn with_store<T>(&self, f: impl FnOnce(&mut dyn Store) -> Result<T, StoreError>) -> Result<T> {
        let mut guard = acquire(&self.store, "store")?;
        Ok(f(guard.as_mut())?)
    }

    fn timeline(&self) -> Result<Vec<Event>> {
        let guard = acquire(&self.store, "store")?;
        Ok(guard.read_timeline(&JobId::new("J1"))?)
    }

    fn kinds(&self) -> Result<Vec<&'static str>> {
        Ok(self.timeline()?.iter().map(|e| e.payload.kind()).collect())
    }

    fn job_state(&self) -> Result<JobState> {
        let guard = acquire(&self.store, "store")?;
        guard
            .list_jobs()?
            .first()
            .map(|j| j.state)
            .ok_or_else(|| anyhow!("no job"))
    }

    fn prompts(&self) -> Result<Vec<String>> {
        Ok(acquire(&self.prompts, "prompts")?.clone())
    }
}

fn good() -> Script {
    Script {
        handoff: Some(HANDOFF),
        stdout: OK_RESULT,
        ..Script::default()
    }
}

fn good_review() -> Script {
    Script {
        handoff: Some(HANDOFF),
        review: Some("## Findings\n\n- [non-blocking] nit\n"),
        stdout: OK_RESULT,
        ..Script::default()
    }
}

fn blocking_review() -> Script {
    Script {
        handoff: Some(HANDOFF),
        review: Some("## Findings\n\n- [blocking] broken\n"),
        stdout: OK_RESULT,
        ..Script::default()
    }
}

fn bad_handoff() -> Script {
    Script {
        handoff: Some("not a handoff"),
        stdout: OK_RESULT,
        ..Script::default()
    }
}

const CLAUDE_ALT_OWNER: &str = r#"
[agents.claude-alt]
adapter = "claude"
command = "claude"

[[rules]]
chain = [{ agent = "claude" }, { agent = "claude-alt" }]
"#;

const QUOTA_429: &str = "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429}\n";

fn retry_causes(rig: &Rig) -> Result<Vec<RetryCause>> {
    Ok(rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::StageRetried { cause, .. } => Some(cause),
            _ => None,
        })
        .collect())
}

fn wip_count(rig: &Rig) -> Result<usize> {
    Ok(acquire(&rig.git.wip, "wip")?.len())
}

fn plan_of(rig: &Rig, names: &[&str]) -> Result<()> {
    use pizza_delivery_core::triage::{Resolution, Template, Triage};
    rig.worker().append(
        &JobId::new("J1"),
        None,
        EventPayload::TriageCompleted {
            triage: Triage {
                repo: Resolution {
                    path: "/src/my repo".into(),
                    confidence: 100,
                    alternatives: vec![],
                },
                template: Template::BugFix,
                plan: names
                    .iter()
                    .map(|n| PlannedStage {
                        name: (*n).into(),
                        reason: "r".into(),
                    })
                    .collect(),
                risk_flags: vec![],
                task: "t".into(),
            },
        },
    )
}

mod decider;
mod memorize;
mod merge;
mod pipeline;
mod quota;
mod resilience;
mod ship;
