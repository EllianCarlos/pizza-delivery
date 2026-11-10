use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pizza_delivery_core::claude::{parse_stream, Outcome};
use pizza_delivery_core::event::{Actor, EventPayload};
use pizza_delivery_core::handoff::validate;
use pizza_delivery_core::prompt::{render_stage_prompt, TEMPLATE_NAME, TEMPLATE_VERSION};
use pizza_delivery_core::{
    fold_stages, ClaimRequest, Claimed, JobId, NewEvent, Store, StoreError, MAX_ATTEMPTS,
};
use sha2::{Digest, Sha256};

use crate::agent::{Agent, Invocation};
use crate::git::Git;
use crate::handler::Env;
use crate::process::{kill_group, start_ticks};

pub struct WorkerConfig {
    pub data_dir: PathBuf,
    pub holder: String,
    pub lease_ttl_ms: i64,
    pub heartbeat_ms: u64,
    pub timeout: Duration,
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
    Retry(String),
    Fail(String),
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

    pub fn tick(&self) -> Result<bool> {
        self.reap()?;
        let request = ClaimRequest {
            holder: self.config.holder.clone(),
            now_ms: self.env.now_ms(),
            ttl_ms: self.config.lease_ttl_ms,
            stage_id: self.env.new_ulid(),
            event_ids: [self.env.new_ulid(), self.env.new_ulid()],
        };
        match self.with_store(|s| s.claim_stage(request))? {
            Some(claimed) => self.run_claimed(&claimed).map(|()| true),
            None => Ok(false),
        }
    }

    fn reap(&self) -> Result<()> {
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
            let attempt = fold_stages(&events)
                .iter()
                .find(|s| s.id == lease.stage_id)
                .map_or(MAX_ATTEMPTS, |s| s.attempt);
            let verdict = if attempt < MAX_ATTEMPTS {
                Verdict::Retry("lease expired".to_owned())
            } else {
                Verdict::Fail("lease expired and attempts are exhausted".to_owned())
            };
            self.settle(&lease.job_id, &lease.stage_id, verdict, None)?;
        }
        Ok(())
    }

    fn settle(
        &self,
        job: &JobId,
        stage: &str,
        verdict: Verdict,
        release: Option<&str>,
    ) -> Result<()> {
        let stage = Some(stage);
        match verdict {
            Verdict::Succeeded => {
                self.append(job, stage, EventPayload::StageSucceeded {})?;
                self.append(job, None, EventPayload::JobCompleted {})?;
            }
            Verdict::Retry(reason) => {
                self.append(job, stage, EventPayload::StageRetried { reason })?;
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
        let verdict = match self.execute(claimed)? {
            Verdict::Retry(reason) if claimed.attempt >= MAX_ATTEMPTS => {
                Verdict::Fail(format!("{reason} (attempts exhausted)"))
            }
            other => other,
        };
        self.settle(
            &claimed.job.id,
            &claimed.stage_id,
            verdict,
            Some(&self.config.holder),
        )
    }

    fn record_artifact(&self, claimed: &Claimed, kind: &str, relative: &Path) -> Result<()> {
        let bytes = std::fs::read(self.config.data_dir.join(relative))?;
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
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
        let worktree = self
            .config
            .data_dir
            .join("worktrees")
            .join(slug(&repo))
            .join(job.id.as_str());
        if let Err(e) = self
            .git
            .ensure_worktree(&repo, &worktree, &format!("pizza/{}", job.id))
        {
            return Ok(Verdict::Fail(format!("worktree: {e:#}")));
        }
        let relative = PathBuf::from("jobs")
            .join(job.id.as_str())
            .join("stages")
            .join(format!("{}-{}", claimed.n, claimed.name));
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
        let prompt = render_stage_prompt(
            job.id.as_str(),
            &claimed.name,
            &job.prompt,
            &handoff_path.to_string_lossy(),
        );
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
            timeout: self.config.timeout,
        };
        let started = self.env.now_ms();
        let stop = AtomicBool::new(false);
        let output = std::thread::scope(|scope| {
            let beat = scope.spawn(|| self.beat(&claimed.stage_id, &stop));
            let result = self.agent.run(&invocation, &mut |pid| {
                let start = start_ticks(pid).unwrap_or_default();
                let _ = self.with_store(|s| s.set_lease_process(&claimed.stage_id, pid, start));
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
        self.record_artifact(claimed, "raw-output", &raw_rel)?;
        let run = parse_stream(&output.stdout);
        let mut usage = run.usage;
        usage.wall_ms = wall_ms;
        self.append(&job.id, stage, EventPayload::AgentFinished { usage })?;
        if output.timed_out {
            return Ok(Verdict::Retry("agent timed out".to_owned()));
        }
        match run.outcome {
            Outcome::Failed {
                reason,
                retryable: true,
            } => Ok(Verdict::Retry(reason)),
            Outcome::Failed { reason, .. } => Ok(Verdict::Fail(reason)),
            Outcome::Succeeded => self.accept_handoff(claimed, &handoff_rel),
        }
    }

    fn accept_handoff(&self, claimed: &Claimed, handoff_rel: &Path) -> Result<Verdict> {
        let path = self.config.data_dir.join(handoff_rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Verdict::Retry("agent wrote no handoff".to_owned()));
        };
        self.record_artifact(claimed, "handoff", handoff_rel)?;
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
            Err(e) => Ok(Verdict::Retry(format!("invalid handoff: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicI64;

    use pizza_delivery_core::event::Event;
    use pizza_delivery_core::job::JobState;
    use pizza_delivery_core::NewJob;

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

    struct FakeGit;

    impl Git for FakeGit {
        fn ensure_worktree(&self, _repo: &Path, worktree: &Path, _branch: &str) -> Result<()> {
            Ok(std::fs::create_dir_all(worktree)?)
        }
    }

    struct Script {
        handoff: Option<&'static str>,
        stdout: &'static str,
    }

    struct FakeAgent {
        scripts: Mutex<VecDeque<Script>>,
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
            if let Some(text) = script.handoff {
                std::fs::write(&inv.handoff_path, text)?;
            }
            Ok(AgentOutput {
                stdout: script.stdout.to_owned(),
                timed_out: false,
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
                submitted_at_ms: 0,
            })?;
            Ok(Self {
                config: WorkerConfig {
                    data_dir: dir.path().to_owned(),
                    holder: "w1".into(),
                    lease_ttl_ms: 1000,
                    heartbeat_ms: 100,
                    timeout: Duration::from_secs(5),
                },
                _dir: dir,
                store: Mutex::new(Box::new(store)),
                env: FakeEnv {
                    now: AtomicI64::new(100),
                    next: AtomicI64::new(1),
                },
                git: FakeGit,
                agent: FakeAgent {
                    scripts: Mutex::new(scripts.into()),
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
        }
    }

    fn bad_handoff() -> Script {
        Script {
            handoff: Some("not a handoff"),
            stdout: OK_RESULT,
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
        };
        let mut guard = rig.store.lock().map_err(|_| anyhow!("poisoned"))?;
        guard.claim_stage(crashed)?;
        drop(guard);
        rig.env.now.store(5000, Ordering::SeqCst);

        rig.worker().tick()?;

        assert_eq!(
            rig.kinds()?[..6],
            [
                "job-submitted",
                "stage-planned",
                "stage-claimed",
                "lease-expired",
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
}
