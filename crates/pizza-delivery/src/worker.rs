use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pizza_delivery_core::claude::{parse_stream, Outcome};
use pizza_delivery_core::event::{Actor, EventPayload};
use pizza_delivery_core::handoff::validate;
use pizza_delivery_core::prompt::{
    render_stage_prompt, with_interruption_note, TEMPLATE_NAME, TEMPLATE_VERSION,
};
use pizza_delivery_core::{
    classify_failure, fold_stages, is_cancelled, may_retry, render_interruption_note,
    retry_history, stage_timeout, worktree_removable, Caps, ClaimRequest, Claimed, Interruption,
    Job, JobId, JobState, NewEvent, RetryCause, Store, StoreError, Tier, WorktreeFacts,
};
use sha2::{Digest, Sha256};

use crate::agent::{Agent, Invocation};
use crate::git::Git;
use crate::handler::Env;
use crate::process::{kill_group, start_ticks, terminate_group};

pub struct WorkerConfig {
    pub data_dir: PathBuf,
    pub holder: String,
    pub lease_ttl_ms: i64,
    pub heartbeat_ms: u64,
    pub timeout: Option<Duration>,
    pub kill_grace: Duration,
    pub caps: Caps,
    pub retention_ms: i64,
}

pub struct Worker<'a> {
    pub store: &'a Mutex<Box<dyn Store>>,
    pub env: &'a dyn Env,
    pub git: &'a dyn Git,
    pub agent: &'a dyn Agent,
    pub config: &'a WorkerConfig,
}

enum Verdict {
    Succeeded,
    Retry(RetryCause, String),
    Fail(String),
}

struct Target<'a> {
    job: &'a JobId,
    stage_id: &'a str,
    n: u32,
    name: &'a str,
    attempt: u32,
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn slug(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn stage_dir(job: &JobId, n: u32, name: &str) -> PathBuf {
    PathBuf::from("jobs")
        .join(job.as_str())
        .join("stages")
        .join(format!("{n}-{name}"))
}

fn note_path(dir: &Path, attempt: u32) -> PathBuf {
    dir.join(format!("interruption-{attempt}.md"))
}

impl Worker<'_> {
    fn with_store<T>(&self, f: impl FnOnce(&mut dyn Store) -> Result<T, StoreError>) -> Result<T> {
        let mut guard = self
            .store
            .lock()
            .map_err(|_| anyhow!("store lock poisoned"))?;
        Ok(f(guard.as_mut())?)
    }

    fn append(&self, job: &JobId, stage: Option<&str>, payload: EventPayload) -> Result<()> {
        let event = NewEvent {
            id: self.env.new_ulid(),
            job_id: job.clone(),
            stage_id: stage.map(str::to_owned),
            ts_ms: self.env.now_ms(),
            actor: Actor::Daemon,
            payload,
        };
        self.with_store(|s| s.append_event(event)).map(|_| ())
    }

    fn worktree_of(&self, repo: &Path, job: &JobId) -> PathBuf {
        self.config
            .data_dir
            .join("worktrees")
            .join(slug(repo))
            .join(job.as_str())
    }

    fn find_job(&self, id: &JobId) -> Result<Option<Job>> {
        Ok(self
            .with_store(|s| s.list_jobs())?
            .into_iter()
            .find(|j| &j.id == id))
    }

    fn cancelled(&self, job: &JobId) -> bool {
        self.with_store(|s| s.read_timeline(job))
            .is_ok_and(|events| is_cancelled(&events))
    }

    #[cfg(test)]
    pub fn tick(&self) -> Result<bool> {
        self.reap()?;
        self.run_once()
    }

    pub fn run_once(&self) -> Result<bool> {
        let request = ClaimRequest {
            holder: self.config.holder.clone(),
            now_ms: self.env.now_ms(),
            ttl_ms: self.config.lease_ttl_ms,
            stage_id: self.env.new_ulid(),
            event_ids: [self.env.new_ulid(), self.env.new_ulid()],
            agent: self.agent.name().to_owned(),
            caps: self.config.caps,
        };
        match self.with_store(|s| s.claim_stage(request))? {
            Some(claimed) => self.run_claimed(&claimed).map(|()| true),
            None => Ok(false),
        }
    }

    pub fn reap(&self) -> Result<()> {
        let now = self.env.now_ms();
        for lease in self.with_store(|s| s.expired_leases(now))? {
            if let (Some(pid), Some(start)) = (lease.pid, lease.pid_start) {
                if start_ticks(pid) == Some(start) {
                    kill_group(pid);
                }
            }
            let stage = Some(lease.stage_id.as_str());
            self.append(
                &lease.job_id,
                stage,
                EventPayload::LeaseExpired {
                    holder: lease.holder.clone(),
                },
            )?;
            let events = self.with_store(|s| s.read_timeline(&lease.job_id))?;
            if let Some(view) = fold_stages(&events).iter().find(|s| s.id == lease.stage_id) {
                self.interrupt(
                    &Target {
                        job: &lease.job_id,
                        stage_id: &view.id,
                        n: view.n,
                        name: &view.name,
                        attempt: view.attempt,
                    },
                    RetryCause::Crash,
                    "the daemon or the agent died and the lease expired",
                )?;
            }
            self.settle(
                &lease.job_id,
                &lease.stage_id,
                Verdict::Retry(RetryCause::Crash, "lease expired".to_owned()),
                None,
            )?;
        }
        Ok(())
    }

    pub fn sweep(&self) -> Result<usize> {
        let now = self.env.now_ms();
        let mut removed = 0;
        for job in self.with_store(|s| s.list_jobs())? {
            if job.state != JobState::Completed {
                continue;
            }
            let Some(repo) = job.repo.as_deref().map(PathBuf::from) else {
                continue;
            };
            let worktree = self.worktree_of(&repo, &job.id);
            if !worktree.exists() {
                continue;
            }
            let events = self.with_store(|s| s.read_timeline(&job.id))?;
            let finished_at_ms = events
                .iter()
                .rev()
                .find(|e| matches!(e.payload, EventPayload::JobCompleted {}))
                .map(|e| e.ts_ms);
            let facts = WorktreeFacts {
                job_state: job.state,
                finished_at_ms,
                pushed: self.git.is_pushed(&worktree).unwrap_or(false),
                dirty: self.git.is_dirty(&worktree).unwrap_or(true),
            };
            if worktree_removable(facts, now, self.config.retention_ms)
                && self.git.remove_worktree(&repo, &worktree).is_ok()
            {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn interrupt(&self, target: &Target<'_>, cause: RetryCause, detail: &str) -> Result<()> {
        let Some(job) = self.find_job(target.job)? else {
            return Ok(());
        };
        let message = format!(
            "wip: {} attempt {} interrupted ({})",
            target.name,
            target.attempt,
            cause.as_str()
        );
        let (wip, detail) = match job.repo.as_deref().map(PathBuf::from) {
            Some(repo) => {
                match self
                    .git
                    .commit_wip(&self.worktree_of(&repo, target.job), &message)
                {
                    Ok(sha) => (sha, detail.to_owned()),
                    Err(e) => (None, format!("{detail} (WIP commit failed: {e:#})")),
                }
            }
            None => (None, detail.to_owned()),
        };
        let note = render_interruption_note(&Interruption {
            cause,
            stage: target.name,
            attempt: target.attempt,
            detail: &detail,
            wip_commit: wip.as_deref(),
        });
        let relative = note_path(
            &stage_dir(target.job, target.n, target.name),
            target.attempt,
        );
        let path = self.config.data_dir.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, note)?;
        self.record_artifact(target.job, target.stage_id, "interruption-note", &relative)
    }

    fn settle(
        &self,
        job: &JobId,
        stage: &str,
        verdict: Verdict,
        release: Option<&str>,
    ) -> Result<()> {
        let events = self.with_store(|s| s.read_timeline(job))?;
        let verdict = match verdict {
            Verdict::Retry(cause, reason) => {
                if may_retry(cause, &retry_history(&events, stage)) {
                    Verdict::Retry(cause, reason)
                } else {
                    Verdict::Fail(format!("{reason} (retries exhausted)"))
                }
            }
            other => other,
        };
        let stage = Some(stage);
        match verdict {
            Verdict::Succeeded => {
                self.append(job, stage, EventPayload::StageSucceeded {})?;
                self.append(job, None, EventPayload::JobCompleted {})?;
            }
            Verdict::Retry(_, _) | Verdict::Fail(_) if is_cancelled(&events) => {
                self.append(
                    job,
                    stage,
                    EventPayload::StageFailed {
                        reason: "cancelled".to_owned(),
                    },
                )?;
            }
            Verdict::Retry(cause, reason) => {
                self.append(job, stage, EventPayload::StageRetried { reason, cause })?;
            }
            Verdict::Fail(reason) => {
                self.append(
                    job,
                    stage,
                    EventPayload::StageFailed {
                        reason: reason.clone(),
                    },
                )?;
                self.append(job, None, EventPayload::JobFailed { reason })?;
            }
        }
        if let Some(holder) = release {
            self.append(
                job,
                stage,
                EventPayload::LeaseReleased {
                    holder: holder.to_owned(),
                },
            )?;
        }
        Ok(())
    }

    fn run_claimed(&self, claimed: &Claimed) -> Result<()> {
        let verdict = self.execute(claimed)?;
        if let Verdict::Retry(cause, reason) = &verdict {
            if cause.interrupts_work() {
                self.interrupt(
                    &Target {
                        job: &claimed.job.id,
                        stage_id: &claimed.stage_id,
                        n: claimed.n,
                        name: &claimed.name,
                        attempt: claimed.attempt,
                    },
                    *cause,
                    reason,
                )?;
            }
        }
        self.settle(
            &claimed.job.id,
            &claimed.stage_id,
            verdict,
            Some(&self.config.holder),
        )
    }

    fn record_artifact(
        &self,
        job: &JobId,
        stage_id: &str,
        kind: &str,
        relative: &Path,
    ) -> Result<()> {
        let bytes = std::fs::read(self.config.data_dir.join(relative))?;
        self.append(
            job,
            Some(stage_id),
            EventPayload::ArtifactRecorded {
                kind: kind.to_owned(),
                path: relative.to_string_lossy().into_owned(),
                sha256: sha256_hex(&bytes),
            },
        )
    }

    fn beat(&self, stage_id: &str, stop: &AtomicBool) {
        let interval = Duration::from_millis(self.config.heartbeat_ms);
        let mut waited = Duration::ZERO;
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
            waited += Duration::from_millis(10);
            if waited >= interval {
                waited = Duration::ZERO;
                let expires = self.env.now_ms() + self.config.lease_ttl_ms;
                let _ = self.with_store(|s| s.heartbeat(stage_id, expires));
            }
        }
    }

    fn execute(&self, claimed: &Claimed) -> Result<Verdict> {
        let job = &claimed.job;
        let Some(repo) = job.repo.as_deref().map(PathBuf::from) else {
            return Ok(Verdict::Fail("job has no repository".to_owned()));
        };
        let worktree = self.worktree_of(&repo, &job.id);
        if let Err(e) = self
            .git
            .ensure_worktree(&repo, &worktree, &format!("pizza/{}", job.id))
        {
            return Ok(Verdict::Fail(format!("worktree: {e:#}")));
        }
        let relative = stage_dir(&job.id, claimed.n, &claimed.name);
        let dir = self.config.data_dir.join(&relative);
        std::fs::create_dir_all(&dir)?;
        let handoff_rel = relative.join("handoff.md");
        let handoff_path = self.config.data_dir.join(&handoff_rel);
        if handoff_path.exists() {
            std::fs::remove_file(&handoff_path)?;
        }
        let stage = Some(claimed.stage_id.as_str());
        self.append(
            &job.id,
            stage,
            EventPayload::StageStarted {
                attempt: claimed.attempt,
            },
        )?;
        let mut prompt = render_stage_prompt(
            job.id.as_str(),
            &claimed.name,
            &job.prompt,
            &handoff_path.to_string_lossy(),
        );
        if let Some(previous) = claimed.attempt.checked_sub(1).filter(|p| *p > 0) {
            if let Ok(note) =
                std::fs::read_to_string(dir.join(format!("interruption-{previous}.md")))
            {
                prompt = with_interruption_note(&prompt, &note);
            }
        }
        self.append(
            &job.id,
            stage,
            EventPayload::AgentInvoked {
                agent: self.agent.name().to_owned(),
                template: TEMPLATE_NAME.to_owned(),
                version: TEMPLATE_VERSION,
                prompt_sha256: sha256_hex(prompt.as_bytes()),
            },
        )?;
        let invocation = Invocation {
            prompt,
            cwd: worktree,
            handoff_path: handoff_path.clone(),
            timeout: self
                .config
                .timeout
                .unwrap_or_else(|| stage_timeout(&claimed.name, Tier::Medium)),
            grace: self.config.kill_grace,
        };
        let started = self.env.now_ms();
        let stop = AtomicBool::new(false);
        let output = std::thread::scope(|scope| {
            let beat = scope.spawn(|| self.beat(&claimed.stage_id, &stop));
            let result = self.agent.run(&invocation, &mut |pid| {
                let start = start_ticks(pid).unwrap_or_default();
                let _ = self.with_store(|s| s.set_lease_process(&claimed.stage_id, pid, start));
                if self.cancelled(&job.id) {
                    terminate_group(pid);
                }
            });
            stop.store(true, Ordering::SeqCst);
            let _ = beat.join();
            result
        });
        let output = match output {
            Ok(output) => output,
            Err(e) => return Ok(Verdict::Fail(format!("agent: {e:#}"))),
        };
        let wall_ms = u64::try_from(self.env.now_ms() - started).ok();
        let raw_rel = relative.join("raw-output.jsonl");
        std::fs::write(self.config.data_dir.join(&raw_rel), &output.stdout)?;
        self.record_artifact(&job.id, &claimed.stage_id, "raw-output", &raw_rel)?;
        let run = parse_stream(&output.stdout);
        let mut usage = run.usage;
        usage.wall_ms = wall_ms;
        self.append(&job.id, stage, EventPayload::AgentFinished { usage })?;
        if output.timed_out {
            return Ok(Verdict::Retry(
                RetryCause::Timeout,
                "agent timed out".to_owned(),
            ));
        }
        match run.outcome {
            Outcome::Failed {
                reason,
                retryable: true,
            } => Ok(Verdict::Retry(classify_failure(&reason), reason)),
            Outcome::Failed { reason, .. } => Ok(Verdict::Fail(reason)),
            Outcome::Succeeded => self.accept_handoff(claimed, &handoff_rel),
        }
    }

    fn accept_handoff(&self, claimed: &Claimed, handoff_rel: &Path) -> Result<Verdict> {
        let path = self.config.data_dir.join(handoff_rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                "agent wrote no handoff".to_owned(),
            ));
        };
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "handoff", handoff_rel)?;
        match validate(&text) {
            Ok(_) => {
                self.append(
                    &claimed.job.id,
                    Some(&claimed.stage_id),
                    EventPayload::HandoffWritten {
                        path: handoff_rel.to_string_lossy().into_owned(),
                    },
                )?;
                Ok(Verdict::Succeeded)
            }
            Err(e) => Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                format!("invalid handoff: {e}"),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicI64;

    use pizza_delivery_core::event::Event;
    use pizza_delivery_core::job::JobState;
    use pizza_delivery_core::{NewJob, Priority};
    use std::sync::atomic::AtomicBool;

    use super::*;
    use crate::agent::AgentOutput;
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
    }

    impl Git for FakeGit {
        fn ensure_worktree(&self, _repo: &Path, worktree: &Path, _branch: &str) -> Result<()> {
            Ok(std::fs::create_dir_all(worktree)?)
        }
        fn commit_wip(&self, _worktree: &Path, message: &str) -> Result<Option<String>> {
            self.wip
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .push(message.to_owned());
            Ok(Some("wip1".to_owned()))
        }
        fn is_dirty(&self, _worktree: &Path) -> Result<bool> {
            Ok(self.dirty.load(Ordering::SeqCst))
        }
        fn is_pushed(&self, _worktree: &Path) -> Result<bool> {
            Ok(self.pushed.load(Ordering::SeqCst))
        }
        fn remove_worktree(&self, _repo: &Path, worktree: &Path) -> Result<()> {
            self.removed
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .push(worktree.to_owned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct Script {
        handoff: Option<&'static str>,
        stdout: &'static str,
        timed_out: bool,
    }

    struct FakeAgent {
        scripts: Mutex<VecDeque<Script>>,
        prompts: Mutex<Vec<String>>,
    }

    impl Agent for FakeAgent {
        fn name(&self) -> &str {
            "fake"
        }
        fn run(&self, inv: &Invocation, _on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput> {
            let script = self
                .scripts
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .pop_front()
                .ok_or_else(|| anyhow!("no scripted run left"))?;
            self.prompts
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .push(inv.prompt.clone());
            if let Some(text) = script.handoff {
                std::fs::write(&inv.handoff_path, text)?;
            }
            Ok(AgentOutput {
                stdout: script.stdout.to_owned(),
                timed_out: script.timed_out,
            })
        }
    }

    struct Rig {
        _dir: tempfile::TempDir,
        store: Mutex<Box<dyn Store>>,
        env: FakeEnv,
        git: FakeGit,
        agent: FakeAgent,
        config: WorkerConfig,
    }

    impl Rig {
        fn new(scripts: Vec<Script>) -> Result<Self> {
            let dir = tempfile::TempDir::new()?;
            let mut store = SqliteStore::open(&dir.path().join("l.db"))?;
            store.enqueue_job(NewJob {
                id: JobId::new("J1"),
                event_id: "E0".into(),
                prompt: "do it".into(),
                repo: Some("/src/my repo".into()),
                priority: Priority::Normal,
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
                },
                _dir: dir,
                store: Mutex::new(Box::new(store)),
                env: FakeEnv {
                    now: AtomicI64::new(100),
                    next: AtomicI64::new(1),
                },
                git: FakeGit::default(),
                agent: FakeAgent {
                    scripts: Mutex::new(scripts.into()),
                    prompts: Mutex::default(),
                },
            })
        }

        fn worker(&self) -> Worker<'_> {
            Worker {
                store: &self.store,
                env: &self.env,
                git: &self.git,
                agent: &self.agent,
                config: &self.config,
            }
        }

        fn timeline(&self) -> Result<Vec<Event>> {
            let guard = self.store.lock().map_err(|_| anyhow!("poisoned"))?;
            Ok(guard.read_timeline(&JobId::new("J1"))?)
        }

        fn kinds(&self) -> Result<Vec<&'static str>> {
            Ok(self.timeline()?.iter().map(|e| e.payload.kind()).collect())
        }

        fn job_state(&self) -> Result<JobState> {
            let guard = self.store.lock().map_err(|_| anyhow!("poisoned"))?;
            guard
                .list_jobs()?
                .first()
                .map(|j| j.state)
                .ok_or_else(|| anyhow!("no job"))
        }
    }

    fn good() -> Script {
        Script {
            handoff: Some(HANDOFF),
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

    #[test]
    fn a_valid_handoff_completes_the_job_and_the_whole_run_is_ledgered() -> Result<()> {
        let rig = Rig::new(vec![good()])?;

        assert!(rig.worker().tick()?);

        assert_eq!(
            rig.kinds()?,
            vec![
                "job-submitted",
                "stage-planned",
                "stage-claimed",
                "stage-started",
                "agent-invoked",
                "artifact-recorded",
                "agent-finished",
                "artifact-recorded",
                "handoff-written",
                "stage-succeeded",
                "job-completed",
                "lease-released",
            ]
        );
        assert_eq!(rig.job_state()?, JobState::Completed);
        assert!(!rig.worker().tick()?);
        Ok(())
    }

    #[test]
    fn artifacts_are_indexed_by_kind_relative_path_and_sha256() -> Result<()> {
        let rig = Rig::new(vec![good()])?;

        rig.worker().tick()?;

        let artifacts: Vec<(String, String, String)> = rig
            .timeline()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::ArtifactRecorded { kind, path, sha256 } => Some((kind, path, sha256)),
                _ => None,
            })
            .collect();
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].0, "raw-output");
        assert_eq!(
            artifacts[0].1,
            "jobs/J1/stages/1-implement/raw-output.jsonl"
        );
        assert_eq!(
            artifacts[0].2,
            "0f3c8b90bcba63a3516fae9fd3e5d103f5c506fcc0e4fcbd7cfd280c024232a5"
        );
        assert_eq!(artifacts[1].0, "handoff");
        assert_eq!(artifacts[1].1, "jobs/J1/stages/1-implement/handoff.md");
        assert_eq!(artifacts[1].2, HANDOFF_SHA256);
        Ok(())
    }

    #[test]
    fn an_invalid_handoff_is_retried_once_and_then_succeeds() -> Result<()> {
        let rig = Rig::new(vec![bad_handoff(), good()])?;

        rig.worker().tick()?;
        assert_eq!(rig.job_state()?, JobState::Queued);
        rig.worker().tick()?;

        let kinds = rig.kinds()?;
        assert!(kinds.contains(&"stage-retried"));
        assert!(!kinds.contains(&"stage-failed"));
        assert_eq!(rig.job_state()?, JobState::Completed);
        Ok(())
    }

    #[test]
    fn a_handoff_that_stays_invalid_fails_the_stage_and_the_job() -> Result<()> {
        let rig = Rig::new(vec![bad_handoff(), bad_handoff()])?;

        rig.worker().tick()?;
        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::Failed);
        let failure = rig.timeline()?.into_iter().find_map(|e| match e.payload {
            EventPayload::StageFailed { reason } => Some(reason),
            _ => None,
        });
        assert!(failure.is_some_and(|r| r.contains("invalid handoff")));
        Ok(())
    }

    #[test]
    fn a_missing_handoff_counts_as_invalid() -> Result<()> {
        let rig = Rig::new(vec![
            Script {
                handoff: None,
                stdout: OK_RESULT,
                ..Script::default()
            },
            good(),
        ])?;

        rig.worker().tick()?;
        rig.worker().tick()?;

        assert!(rig.kinds()?.contains(&"stage-retried"));
        assert_eq!(rig.job_state()?, JobState::Completed);
        Ok(())
    }

    #[test]
    fn a_provider_error_fails_without_retrying_even_if_a_handoff_exists() -> Result<()> {
        let rig = Rig::new(vec![Script {
            handoff: Some(HANDOFF),
            stdout: ERR_RESULT,
            ..Script::default()
        }])?;

        rig.worker().tick()?;

        let kinds = rig.kinds()?;
        assert!(!kinds.contains(&"stage-retried"));
        assert!(kinds.contains(&"stage-failed"));
        assert_eq!(rig.job_state()?, JobState::Failed);
        Ok(())
    }

    #[test]
    fn usage_is_recorded_with_unknown_values_left_empty() -> Result<()> {
        let rig = Rig::new(vec![good()])?;

        rig.worker().tick()?;

        let usage = rig.timeline()?.into_iter().find_map(|e| match e.payload {
            EventPayload::AgentFinished { usage } => Some(usage),
            _ => None,
        });
        let usage = usage.ok_or_else(|| anyhow!("no agent-finished event"))?;
        assert_eq!(usage.model, None);
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cost_microusd, None);
        assert_eq!(usage.wall_ms, Some(0));
        Ok(())
    }

    #[test]
    fn an_expired_lease_is_ledgered_and_the_stage_is_reclaimed_as_attempt_two() -> Result<()> {
        let rig = Rig::new(vec![good()])?;
        let crashed = ClaimRequest {
            holder: "crashed-daemon".into(),
            now_ms: 100,
            ttl_ms: 1000,
            stage_id: "S0".into(),
            event_ids: ["EP0".into(), "EC0".into()],
            agent: "fake".into(),
            caps: Caps::default(),
        };
        let mut guard = rig.store.lock().map_err(|_| anyhow!("poisoned"))?;
        guard.claim_stage(crashed)?;
        drop(guard);
        rig.env.now.store(5000, Ordering::SeqCst);

        rig.worker().tick()?;

        assert_eq!(
            rig.kinds()?[..7],
            [
                "job-submitted",
                "stage-planned",
                "stage-claimed",
                "lease-expired",
                "artifact-recorded",
                "stage-retried",
                "stage-claimed"
            ]
        );
        let attempts: Vec<u32> = rig
            .timeline()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::StageClaimed { attempt, .. } => Some(attempt),
                _ => None,
            })
            .collect();
        assert_eq!(attempts, vec![1, 2]);
        assert_eq!(rig.job_state()?, JobState::Completed);
        Ok(())
    }

    #[test]
    fn a_lease_that_expires_on_the_last_attempt_fails_the_job() -> Result<()> {
        let rig = Rig::new(vec![])?;
        let claim = |tag: &str, now_ms| ClaimRequest {
            holder: "crashed".into(),
            now_ms,
            ttl_ms: 10,
            stage_id: format!("S{tag}"),
            event_ids: [format!("P{tag}"), format!("C{tag}")],
            agent: "fake".into(),
            caps: Caps::default(),
        };
        let mut guard = rig.store.lock().map_err(|_| anyhow!("poisoned"))?;
        guard.claim_stage(claim("0", 0))?;
        drop(guard);
        rig.env.now.store(1000, Ordering::SeqCst);
        rig.worker().reap()?;
        let mut guard = rig.store.lock().map_err(|_| anyhow!("poisoned"))?;
        guard.claim_stage(claim("1", 1000))?;
        drop(guard);
        rig.env.now.store(5000, Ordering::SeqCst);

        rig.worker().reap()?;

        assert_eq!(rig.job_state()?, JobState::Failed);
        Ok(())
    }

    fn hung() -> Script {
        Script {
            timed_out: true,
            ..Script::default()
        }
    }

    fn flaky() -> Script {
        Script::default()
    }

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
        Ok(rig.git.wip.lock().map_err(|_| anyhow!("poisoned"))?.len())
    }

    #[test]
    fn a_timeout_commits_wip_writes_a_note_and_is_retried_once_then_fails() -> Result<()> {
        let rig = Rig::new(vec![hung(), hung()])?;

        rig.worker().tick()?;
        assert_eq!(rig.job_state()?, JobState::Queued);
        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::Failed);
        assert_eq!(retry_causes(&rig)?, vec![RetryCause::Timeout]);
        assert_eq!(wip_count(&rig)?, 2);
        let note = std::fs::read_to_string(
            rig.config
                .data_dir
                .join("jobs/J1/stages/1-implement/interruption-1.md"),
        )?;
        assert!(note.contains("timeout"));
        assert!(note.contains("wip1"));
        assert!(rig.kinds()?.contains(&"artifact-recorded"));
        Ok(())
    }

    #[test]
    fn the_attempt_after_an_interruption_is_given_the_note() -> Result<()> {
        let rig = Rig::new(vec![hung(), good()])?;

        rig.worker().tick()?;
        rig.worker().tick()?;

        let prompts = rig.agent.prompts.lock().map_err(|_| anyhow!("poisoned"))?;
        assert!(!prompts[0].contains("Interruption note"));
        assert!(prompts[1].contains("Interruption note"));
        assert!(prompts[1].contains("wip1"));
        drop(prompts);
        assert_eq!(rig.job_state()?, JobState::Completed);
        Ok(())
    }

    #[test]
    fn transient_failures_are_retried_twice() -> Result<()> {
        let rig = Rig::new(vec![flaky(), flaky(), good()])?;

        for _ in 0..3 {
            rig.worker().tick()?;
        }

        assert_eq!(rig.job_state()?, JobState::Completed);
        assert_eq!(
            retry_causes(&rig)?,
            vec![RetryCause::Transient, RetryCause::Transient]
        );
        assert_eq!(wip_count(&rig)?, 0);
        Ok(())
    }

    #[test]
    fn a_third_transient_failure_is_final() -> Result<()> {
        let rig = Rig::new(vec![flaky(), flaky(), flaky()])?;

        for _ in 0..3 {
            rig.worker().tick()?;
        }

        assert_eq!(rig.job_state()?, JobState::Failed);
        Ok(())
    }

    #[test]
    fn a_crash_leaves_a_note_and_a_wip_commit_for_the_next_attempt() -> Result<()> {
        let rig = Rig::new(vec![good()])?;
        let crashed = ClaimRequest {
            holder: "crashed-daemon".into(),
            now_ms: 100,
            ttl_ms: 1000,
            stage_id: "S0".into(),
            event_ids: ["EP0".into(), "EC0".into()],
            agent: "fake".into(),
            caps: Caps::default(),
        };
        rig.store
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .claim_stage(crashed)?;
        rig.env.now.store(5000, Ordering::SeqCst);

        rig.worker().tick()?;

        let note = std::fs::read_to_string(
            rig.config
                .data_dir
                .join("jobs/J1/stages/1-implement/interruption-1.md"),
        )?;
        assert!(note.contains("crash"));
        assert_eq!(wip_count(&rig)?, 1);
        assert_eq!(retry_causes(&rig)?, vec![RetryCause::Crash]);
        let prompts = rig.agent.prompts.lock().map_err(|_| anyhow!("poisoned"))?;
        assert!(prompts[0].contains("Interruption note"));
        Ok(())
    }

    fn completed_rig(pushed: bool, dirty: bool) -> Result<Rig> {
        let rig = Rig::new(vec![good()])?;
        rig.worker().tick()?;
        rig.git.pushed.store(pushed, Ordering::SeqCst);
        rig.git.dirty.store(dirty, Ordering::SeqCst);
        Ok(rig)
    }

    fn removed(rig: &Rig) -> Result<usize> {
        Ok(rig
            .git
            .removed
            .lock()
            .map_err(|_| anyhow!("poisoned"))?
            .len())
    }

    #[test]
    fn a_clean_pushed_worktree_is_removed_after_retention() -> Result<()> {
        let rig = completed_rig(true, false)?;
        rig.env.now.store(5000, Ordering::SeqCst);

        assert_eq!(rig.worker().sweep()?, 1);
        assert_eq!(removed(&rig)?, 1);
        Ok(())
    }

    #[test]
    fn a_worktree_inside_the_retention_period_is_kept() -> Result<()> {
        let rig = completed_rig(true, false)?;
        rig.env.now.store(500, Ordering::SeqCst);

        assert_eq!(rig.worker().sweep()?, 0);
        Ok(())
    }

    #[test]
    fn dirty_or_unpushed_worktrees_are_never_removed() -> Result<()> {
        for (pushed, dirty) in [(true, true), (false, false), (false, true)] {
            let rig = completed_rig(pushed, dirty)?;
            rig.env.now.store(99_999_999, Ordering::SeqCst);

            assert_eq!(rig.worker().sweep()?, 0);
            assert_eq!(removed(&rig)?, 0);
        }
        Ok(())
    }

    #[test]
    fn a_failed_jobs_worktree_is_never_removed() -> Result<()> {
        let rig = Rig::new(vec![Script {
            handoff: Some(HANDOFF),
            stdout: ERR_RESULT,
            ..Script::default()
        }])?;
        rig.worker().tick()?;
        rig.git.pushed.store(true, Ordering::SeqCst);
        rig.env.now.store(99_999_999, Ordering::SeqCst);

        assert_eq!(rig.job_state()?, JobState::Failed);
        assert_eq!(rig.worker().sweep()?, 0);
        Ok(())
    }
}
