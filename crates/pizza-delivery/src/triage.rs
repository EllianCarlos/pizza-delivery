use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use pizza_delivery_core::budget::{estimate_overrun, estimate_plan, samples, Defaults, Sample};
use pizza_delivery_core::context::{assemble_context, Context, Section, CONTEXT_CAP_BYTES};
use pizza_delivery_core::event::{Actor, EventPayload};
use pizza_delivery_core::job::JobState;
use pizza_delivery_core::sandbox::SandboxProfile;
use pizza_delivery_core::triage::{
    conclude_with, parse_choice, reply_text, resolve_repo, tiebreak_prompt, Candidate, Outcome,
    Project, Triage, TriageOptions,
};
use pizza_delivery_core::{Job, JobId, NewEvent, Store, StoreError};
use sha2::{Digest, Sha256};

use crate::agent::{Agent, Invocation};
use crate::handler::Env;
use crate::lock::acquire;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);
const TIE_BREAK_TIMEOUT: Duration = Duration::from_secs(120);
const LEDGER_HISTORY: usize = 5;
const CONVENTION_FILES: [&str; 5] = [
    "AGENTS.md",
    "CLAUDE.md",
    "CONTRIBUTING.md",
    "GLOSSARY.md",
    "docs/adr",
];

pub trait ProjectIndex: Send + Sync {
    fn projects(&self) -> Vec<Project>;
}

pub trait TieBreaker: Send + Sync {
    fn choose(&self, prompt: &str, top: &[Candidate]) -> Option<usize>;
}

pub trait ContextSource: Send + Sync {
    fn name(&self) -> &str;
    fn read(&self, repo: &Path, prompt: &str) -> Option<String>;
}

pub struct Deps<'a> {
    pub index: &'a dyn ProjectIndex,
    pub tie_breaker: &'a dyn TieBreaker,
    pub sources: Vec<&'a dyn ContextSource>,
}

pub struct Report {
    pub outcome: Outcome,
    pub context: Option<Context>,
}

pub fn analyze(
    prompt: &str,
    repo_flag: Option<&str>,
    options: Option<&TriageOptions>,
    deps: &Deps<'_>,
) -> Report {
    let index = deps.index.projects();
    let resolution = resolve_repo(repo_flag, prompt, &index, &|top| {
        deps.tie_breaker.choose(prompt, top)
    });
    let outcome = conclude_with(prompt, resolution, options);
    let context = outcome.triage.as_ref().map(|t| {
        let repo = Path::new(&t.repo.path);
        let sections: Vec<Section> = deps
            .sources
            .iter()
            .filter_map(|source| {
                source.read(repo, prompt).map(|text| Section {
                    source: source.name().to_owned(),
                    text,
                })
            })
            .collect();
        assemble_context(&sections, CONTEXT_CAP_BYTES)
    });
    Report { outcome, context }
}

pub struct Triager<'a> {
    pub store: &'a Mutex<Box<dyn Store>>,
    pub env: &'a dyn Env,
    pub data_dir: &'a Path,
    pub deps: Deps<'a>,
    pub agent: &'a str,
    pub budget_cap_tokens: u64,
    pub budget_defaults: Defaults,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

impl Triager<'_> {
    fn with_store<T>(&self, f: impl FnOnce(&mut dyn Store) -> Result<T, StoreError>) -> Result<T> {
        let mut guard = acquire(self.store, "store")?;
        Ok(f(guard.as_mut())?)
    }

    fn append(&self, job: &JobId, payload: EventPayload) -> Result<()> {
        let event = NewEvent {
            id: self.env.new_ulid(),
            job_id: job.clone(),
            stage_id: None,
            ts_ms: self.env.now_ms(),
            actor: Actor::Daemon,
            payload,
        };
        self.with_store(|s| s.append_event(event)).map(|_| ())
    }

    fn pending(&self) -> Result<Option<Job>> {
        let jobs = self.with_store(|s| s.list_jobs())?;
        for job in jobs.into_iter().filter(|j| j.state == JobState::Queued) {
            let events = self.with_store(|s| s.read_timeline(&job.id))?;
            let done = events.iter().any(|e| {
                matches!(
                    e.payload,
                    EventPayload::TriageCompleted { .. } | EventPayload::JobNeedsHuman { .. }
                )
            });
            if !done {
                return Ok(Some(job));
            }
        }
        Ok(None)
    }

    pub fn tick(&self) -> Result<bool> {
        let Some(job) = self.pending()? else {
            return Ok(false);
        };
        let report = analyze(
            &job.prompt,
            job.repo.as_deref(),
            job.options.as_ref(),
            &self.deps,
        );
        self.record(&job, &report)?;
        Ok(true)
    }

    fn record(&self, job: &Job, report: &Report) -> Result<()> {
        if let (Some(context), Some(_)) = (&report.context, &report.outcome.triage) {
            self.record_context(&job.id, context)?;
        }
        for f in &report.outcome.feedback {
            self.append(
                &job.id,
                EventPayload::FeedbackRecorded {
                    kind: f.kind.clone(),
                    severity: f.severity.clone(),
                    text: f.text.clone(),
                    confidence: f.confidence,
                    alternatives: f.alternatives.clone(),
                    impact: f.impact.clone(),
                    evidence: f.evidence.clone(),
                },
            )?;
        }
        match &report.outcome.triage {
            Some(triage) => {
                self.record_budget(&job.id, triage)?;
                self.append(
                    &job.id,
                    EventPayload::TriageCompleted {
                        triage: triage.clone(),
                    },
                )
            }
            None => self.append(
                &job.id,
                EventPayload::JobNeedsHuman {
                    reason: "no repository could be resolved".to_owned(),
                },
            ),
        }
    }

    fn history(&self) -> Result<Vec<Sample>> {
        let jobs = self.with_store(|s| s.list_jobs())?;
        let mut all = Vec::new();
        for job in jobs {
            let events = self.with_store(|s| s.read_timeline(&job.id))?;
            all.extend(samples(&events));
        }
        Ok(all)
    }

    fn record_budget(&self, job: &JobId, triage: &Triage) -> Result<()> {
        let estimates = estimate_plan(
            &triage.plan,
            self.agent,
            &self.history()?,
            &self.budget_defaults,
        );
        if let Some(f) = estimate_overrun(&estimates, self.budget_cap_tokens) {
            self.append(
                job,
                EventPayload::FeedbackRecorded {
                    kind: f.kind,
                    severity: f.severity,
                    text: f.text,
                    confidence: f.confidence,
                    alternatives: f.alternatives,
                    impact: f.impact,
                    evidence: f.evidence,
                },
            )?;
        }
        self.append(job, EventPayload::BudgetEstimated { stages: estimates })
    }

    fn record_context(&self, job: &JobId, context: &Context) -> Result<()> {
        let relative = PathBuf::from("jobs").join(job.as_str()).join("context.md");
        let path = self.data_dir.join(&relative);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, &context.text)?;
        for used in &context.included {
            self.append(
                job,
                EventPayload::ContextRead {
                    source: used.source.clone(),
                    bytes: u64::try_from(used.bytes).unwrap_or(u64::MAX),
                },
            )?;
        }
        self.append(
            job,
            EventPayload::ArtifactRecorded {
                kind: "context".to_owned(),
                path: relative.to_string_lossy().into_owned(),
                sha256: sha256_hex(context.text.as_bytes()),
            },
        )
    }
}

fn capture(command: &Path, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let mut child = cmd.spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if started.elapsed() < COMMAND_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let text = reader.join().unwrap_or_default();
    status
        .filter(|s| s.success())
        .map(|_| text)
        .filter(|t| !t.trim().is_empty())
}

pub struct SystemIndex {
    pub projects_dir: PathBuf,
    pub engram_db: PathBuf,
}

fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn project_at(dir: &Path) -> Option<Project> {
    let path = dir.canonicalize().ok()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    Some(Project {
        name,
        path: path.to_string_lossy().into_owned(),
    })
}

fn engram_directories(db: &Path) -> Vec<PathBuf> {
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        db,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return vec![];
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT directory FROM sessions WHERE directory IS NOT NULL")
    else {
        return vec![];
    };
    let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) else {
        return vec![];
    };
    rows.filter_map(Result::ok).map(PathBuf::from).collect()
}

impl ProjectIndex for SystemIndex {
    fn projects(&self) -> Vec<Project> {
        let scanned: Vec<PathBuf> = std::fs::read_dir(&self.projects_dir)
            .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default();
        let mut projects: Vec<Project> = engram_directories(&self.engram_db)
            .into_iter()
            .chain(scanned)
            .filter(|dir| dir.is_dir() && is_repo(dir))
            .filter_map(|dir| project_at(&dir))
            .collect();
        projects.sort_by(|a, b| a.path.cmp(&b.path));
        projects.dedup_by(|a, b| a.path == b.path);
        projects
    }
}

pub struct AgentTieBreaker<'a> {
    pub agent: &'a dyn Agent,
    pub cwd: PathBuf,
    pub grace: Duration,
}

impl TieBreaker for AgentTieBreaker<'_> {
    fn choose(&self, prompt: &str, top: &[Candidate]) -> Option<usize> {
        let invocation = Invocation {
            prompt: tiebreak_prompt(prompt, top),
            model: None,
            cwd: self.cwd.clone(),
            handoff_path: self.cwd.join("tie-break-handoff.md"),
            timeout: TIE_BREAK_TIMEOUT,
            grace: self.grace,
            sandbox: SandboxProfile::default(),
            egress_log: self.cwd.join("tie-break-egress.log"),
            mcp_config: None,
        };
        let output = self.agent.run(&invocation, &mut |_| {}).ok()?;
        if output.timed_out {
            return None;
        }
        parse_choice(&reply_text(&output.stdout), top.len())
    }
}

pub struct EngramSource {
    pub command: PathBuf,
}

impl ContextSource for EngramSource {
    fn name(&self) -> &str {
        "engram"
    }

    fn read(&self, repo: &Path, prompt: &str) -> Option<String> {
        let project = repo.file_name()?.to_string_lossy().into_owned();
        capture(
            &self.command,
            &["search", prompt, "--project", &project, "--limit", "5"],
            None,
        )
    }
}

pub struct MnemonSource {
    pub command: PathBuf,
}

impl ContextSource for MnemonSource {
    fn name(&self) -> &str {
        "mnemon"
    }

    fn read(&self, _repo: &Path, _prompt: &str) -> Option<String> {
        capture(&self.command, &["recall", "preference"], None)
    }
}

pub struct LedgerSource<'a> {
    pub store: &'a Mutex<Box<dyn Store>>,
}

impl ContextSource for LedgerSource<'_> {
    fn name(&self) -> &str {
        "ledger"
    }

    fn read(&self, repo: &Path, _prompt: &str) -> Option<String> {
        let jobs = self.store.lock().ok()?.list_jobs().ok()?;
        let mut recent: Vec<Job> = jobs
            .into_iter()
            .filter(|j| j.repo.as_deref().map(Path::new) == Some(repo))
            .collect();
        recent.sort_by_key(|a| std::cmp::Reverse(a.submitted_at_ms));
        let lines: Vec<String> = recent
            .iter()
            .take(LEDGER_HISTORY)
            .map(|j| {
                let first = j.prompt.lines().next().unwrap_or_default();
                format!("- [{}] {}", j.state.as_str(), first)
            })
            .collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    }
}

pub struct ConventionsSource;

impl ContextSource for ConventionsSource {
    fn name(&self) -> &str {
        "conventions"
    }

    fn read(&self, repo: &Path, _prompt: &str) -> Option<String> {
        let found: Vec<String> = CONVENTION_FILES
            .iter()
            .filter(|f| repo.join(f).exists())
            .map(|f| format!("- {}", repo.join(f).display()))
            .collect();
        (!found.is_empty())
            .then(|| format!("Read these before changing code:\n{}", found.join("\n")))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::sqlite_store::SqliteStore;
    use anyhow::anyhow;
    use pizza_delivery_core::budget::DEFAULT_CAP_TOKENS;
    use pizza_delivery_core::{NewJob, Priority};

    struct FakeEnv {
        next: AtomicI64,
    }

    impl Env for FakeEnv {
        fn now_ms(&self) -> i64 {
            100
        }
        fn new_ulid(&self) -> String {
            format!("U{}", self.next.fetch_add(1, Ordering::SeqCst))
        }
    }

    struct FixedIndex(Vec<Project>);

    impl ProjectIndex for FixedIndex {
        fn projects(&self) -> Vec<Project> {
            self.0.clone()
        }
    }

    struct Choose(Option<usize>);

    impl TieBreaker for Choose {
        fn choose(&self, _prompt: &str, _top: &[Candidate]) -> Option<usize> {
            self.0
        }
    }

    struct Fixed(&'static str, &'static str);

    impl ContextSource for Fixed {
        fn name(&self) -> &str {
            self.0
        }
        fn read(&self, _repo: &Path, _prompt: &str) -> Option<String> {
            Some(self.1.to_owned())
        }
    }

    fn project(name: &str) -> Project {
        Project {
            name: name.to_owned(),
            path: format!("/p/{name}"),
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        store: Mutex<Box<dyn Store>>,
        env: FakeEnv,
    }

    impl Rig {
        fn new(prompt: &str, repo: Option<&str>) -> Result<Self> {
            let dir = tempfile::TempDir::new()?;
            let mut store = SqliteStore::open(&dir.path().join("l.db"))?;
            store.enqueue_job(NewJob {
                id: JobId::new("J1"),
                event_id: "E0".into(),
                prompt: prompt.into(),
                repo: repo.map(str::to_owned),
                priority: Priority::Normal,
                follow_up: None,
                options: None,
                submitted_at_ms: 0,
            })?;
            Ok(Self {
                dir,
                store: Mutex::new(Box::new(store)),
                env: FakeEnv {
                    next: AtomicI64::new(1),
                },
            })
        }

        fn triage(&self, index: &FixedIndex, choice: Option<usize>) -> Result<bool> {
            let tie = Choose(choice);
            let engram = Fixed("engram", "obs about the repo");
            let mnemon = Fixed("mnemon", "prefers small commits");
            let triager = Triager {
                store: &self.store,
                env: &self.env,
                data_dir: self.dir.path(),
                deps: Deps {
                    index,
                    tie_breaker: &tie,
                    sources: vec![&engram, &mnemon],
                },
                agent: "claude",
                budget_cap_tokens: DEFAULT_CAP_TOKENS,
                budget_defaults: Defaults::standard(),
            };
            triager.tick()
        }

        fn events(&self) -> Result<Vec<pizza_delivery_core::Event>> {
            let guard = acquire(&self.store, "store")?;
            Ok(guard.read_timeline(&JobId::new("J1"))?)
        }

        fn kinds(&self) -> Result<Vec<&'static str>> {
            Ok(self.events()?.iter().map(|e| e.payload.kind()).collect())
        }

        fn job(&self) -> Result<Job> {
            let guard = acquire(&self.store, "store")?;
            guard
                .list_jobs()?
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("no job"))
        }
    }

    #[test]
    fn an_explicit_repo_is_triaged_with_context_and_context_read_events() -> Result<()> {
        let rig = Rig::new("fix the crash", Some("/work/app"))?;

        assert!(rig.triage(&FixedIndex(vec![]), None)?);

        assert_eq!(
            rig.kinds()?,
            vec![
                "job-submitted",
                "context-read",
                "context-read",
                "artifact-recorded",
                "budget-estimated",
                "triage-completed"
            ]
        );
        let context = std::fs::read_to_string(rig.dir.path().join("jobs/J1/context.md"))?;
        assert!(context.contains("Sources: engram, mnemon"));
        assert!(context.contains("prefers small commits"));
        assert_eq!(rig.job()?.repo.as_deref(), Some("/work/app"));
        assert!(!rig.triage(&FixedIndex(vec![]), None)?);
        Ok(())
    }

    #[test]
    fn an_unresolvable_job_ends_as_needs_human_with_a_feedback_entry() -> Result<()> {
        let rig = Rig::new("do something unrelated", None)?;

        rig.triage(&FixedIndex(vec![project("blog")]), None)?;

        assert_eq!(
            rig.kinds()?,
            vec!["job-submitted", "feedback-recorded", "job-needs-human"]
        );
        assert_eq!(rig.job()?.state, JobState::NeedsHuman);
        Ok(())
    }

    #[test]
    fn a_low_confidence_pick_logs_an_assumption_and_a_model_choice_does_not() -> Result<()> {
        let index = FixedIndex(vec![project("pizza-delivery"), project("pizza-web")]);
        let unsure = Rig::new("fix the pizza ordering", None)?;
        unsure.triage(&index, None)?;
        assert!(unsure.kinds()?.contains(&"feedback-recorded"));
        assert_eq!(unsure.job()?.repo.as_deref(), Some("/p/pizza-delivery"));

        let decided = Rig::new("fix the pizza ordering", None)?;
        decided.triage(&index, Some(1))?;
        assert!(!decided.kinds()?.contains(&"feedback-recorded"));
        assert_eq!(decided.job()?.repo.as_deref(), Some("/p/pizza-web"));
        Ok(())
    }

    #[test]
    fn dry_run_analysis_needs_no_ledger() {
        let tie = Choose(None);
        let index = FixedIndex(vec![]);
        let source = Fixed("engram", "x");
        let report = analyze(
            "add a feature",
            Some("/r"),
            None,
            &Deps {
                index: &index,
                tie_breaker: &tie,
                sources: vec![&source],
            },
        );
        assert_eq!(report.outcome.triage.map(|t| t.plan.len()), Some(5));
        assert_eq!(report.context.map(|c| c.included.len()), Some(1));
    }

    #[test]
    fn the_index_lists_git_repos_under_the_projects_dir_and_engram_sessions() -> Result<()> {
        let dir = tempfile::TempDir::new()?;
        std::fs::create_dir_all(dir.path().join("proj/alpha/.git"))?;
        std::fs::create_dir_all(dir.path().join("proj/not-a-repo"))?;
        std::fs::create_dir_all(dir.path().join("elsewhere/beta/.git"))?;
        let db = dir.path().join("engram.db");
        let conn = rusqlite::Connection::open(&db)?;
        conn.execute("CREATE TABLE sessions (directory TEXT)", [])?;
        conn.execute(
            "INSERT INTO sessions VALUES (?1), (?2)",
            [
                dir.path()
                    .join("elsewhere/beta")
                    .to_string_lossy()
                    .into_owned(),
                dir.path().join("proj/alpha").to_string_lossy().into_owned(),
            ],
        )?;
        let index = SystemIndex {
            projects_dir: dir.path().join("proj"),
            engram_db: db,
        };

        let names: Vec<String> = index.projects().into_iter().map(|p| p.name).collect();

        assert_eq!(names, vec!["beta".to_owned(), "alpha".to_owned()]);
        Ok(())
    }

    #[test]
    fn conventions_point_at_existing_files_and_ledger_lists_recent_jobs() -> Result<()> {
        let dir = tempfile::TempDir::new()?;
        std::fs::write(dir.path().join("AGENTS.md"), "x")?;
        let found = ConventionsSource.read(dir.path(), "p");
        assert!(found.is_some_and(|t| t.contains("AGENTS.md") && !t.contains("CLAUDE.md")));
        assert_eq!(ConventionsSource.read(&dir.path().join("none"), "p"), None);

        let rig = Rig::new("first line\nsecond", Some("/work/app"))?;
        let ledger = LedgerSource { store: &rig.store };
        assert_eq!(
            ledger.read(Path::new("/work/app"), "p"),
            Some("- [queued] first line".to_owned())
        );
        assert_eq!(ledger.read(Path::new("/other"), "p"), None);
        Ok(())
    }
}
