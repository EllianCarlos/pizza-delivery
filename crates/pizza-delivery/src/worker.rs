use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pizza_delivery_core::budget::{
    actual_overrun, estimate_for, estimate_stage, tier_for, Defaults, SizeTier,
};
use pizza_delivery_core::classify::{classify, Classified, Failure, RawRun};
use pizza_delivery_core::event::{Actor, EventPayload};
use pizza_delivery_core::feedback::parse_feedback;
use pizza_delivery_core::grill;
use pizza_delivery_core::handoff::validate;
use pizza_delivery_core::prompt::{render, Rendered, Templates, Vars};
use pizza_delivery_core::quota::{fold_quota, QuotaStatus};
use pizza_delivery_core::review::{self, Decision};
use pizza_delivery_core::routing::{
    route, stage_capabilities, Candidate, RouteRequest, RoutingConfig, Skip,
};
use pizza_delivery_core::sandbox::{blocked_hosts, Mount, SandboxProfile, TrustConfig};
use pizza_delivery_core::spec;
use pizza_delivery_core::stage::PIPELINE_STAGES;
use pizza_delivery_core::triage::{
    amend_plan, current_plan, Feedback, PlannedStage, DEFAULT_MAX_STAGES,
};
use pizza_delivery_core::{
    fix_rounds, fold_stages, is_cancelled, may_retry, pipeline_done, render_interruption_note,
    retry_history, stage_timeout, worktree_removable, Caps, ClaimRequest, Claimed, Interruption,
    Job, JobId, JobState, NewEvent, RetryCause, Store, StoreError, Tier, WorktreeFacts,
};
use sha2::{Digest, Sha256};

use crate::agent::{Fleet, Invocation};
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
    pub trust: TrustConfig,
    pub budget_cap_tokens: u64,
    pub require_triage: bool,
}

pub struct Worker<'a> {
    pub store: &'a Mutex<Box<dyn Store>>,
    pub env: &'a dyn Env,
    pub git: &'a dyn Git,
    pub fleet: &'a Fleet,
    pub routing: &'a RoutingConfig,
    pub config: &'a WorkerConfig,
}

const DEFAULT_TIER: &str = "standard";
const MIN_PARK_MS: i64 = 1_000;

enum Verdict {
    Succeeded,
    NeedsHuman(String),
    Retry(RetryCause, String),
    Fail(String),
    Park { until_ms: i64, reason: String },
}

enum Attempt {
    Settled(Verdict),
    Quota { until_ms: i64, reason: String },
    Provider(String),
}

struct StageRun {
    repo: PathBuf,
    relative: PathBuf,
    handoff_rel: PathBuf,
    handoff_path: PathBuf,
    worktree: PathBuf,
}

struct Target<'a> {
    job: &'a JobId,
    stage_id: &'a str,
    n: u32,
    name: &'a str,
    attempt: u32,
}

impl<'a> Target<'a> {
    fn of(claimed: &'a Claimed) -> Self {
        Self {
            job: &claimed.job.id,
            stage_id: &claimed.stage_id,
            n: claimed.n,
            name: &claimed.name,
            attempt: claimed.attempt,
        }
    }
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

fn spec_dir(job: &JobId) -> String {
    format!("docs/specs/{}", job.as_str())
}

fn note_path(dir: &Path, attempt: u32) -> PathBuf {
    dir.join(format!("interruption-{attempt}.md"))
}

fn stored_note(dir: &Path, attempt: u32) -> Option<String> {
    std::fs::read_to_string(note_path(dir, attempt))
        .ok()
        .or_else(|| {
            attempt
                .checked_sub(1)
                .filter(|previous| *previous > 0)
                .and_then(|previous| std::fs::read_to_string(note_path(dir, previous)).ok())
        })
}

fn tier_of(stage: &str) -> Tier {
    match tier_for(stage) {
        SizeTier::Small => Tier::Small,
        SizeTier::Medium => Tier::Medium,
        SizeTier::Large => Tier::Large,
    }
}

fn retry_cause(failure: &Failure) -> RetryCause {
    match failure {
        Failure::Timeout => RetryCause::Timeout,
        Failure::Crash(_) => RetryCause::Crash,
        Failure::QuotaExhausted { .. } => RetryCause::Quota,
        Failure::Transient(_) | Failure::ProviderError(_) | Failure::Config(_) => {
            RetryCause::Transient
        }
    }
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

    fn planned_agent(&self) -> String {
        self.routing
            .rules
            .first()
            .and_then(|rule| rule.chain.first())
            .map(|candidate| candidate.agent.clone())
            .unwrap_or_default()
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
            require_triage: self.config.require_triage,
            agent: self.planned_agent(),
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
            if let Some(view) = fold_stages(&events)
                .iter()
                .find(|s| s.id == lease.stage_id)
                .cloned()
            {
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

    fn interrupt(&self, target: &Target<'_>, cause: RetryCause, detail: &str) -> Result<String> {
        let Some(job) = self.find_job(target.job)? else {
            return Ok(String::new());
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
        std::fs::write(&path, &note)?;
        self.record_artifact(target.job, target.stage_id, "interruption-note", &relative)?;
        Ok(note)
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
                let events = self.with_store(|s| s.read_timeline(job))?;
                if pipeline_done(&events) {
                    self.append(job, None, EventPayload::JobCompleted {})?;
                }
            }
            Verdict::NeedsHuman(reason) => {
                self.append(
                    job,
                    stage,
                    EventPayload::StageFailed {
                        reason: reason.clone(),
                    },
                )?;
                self.append(job, None, EventPayload::JobNeedsHuman { reason })?;
            }
            Verdict::Retry(_, _) | Verdict::Fail(_) | Verdict::Park { .. }
                if is_cancelled(&events) =>
            {
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
            Verdict::Park { until_ms, reason } => {
                self.append(job, stage, EventPayload::JobParked { until_ms, reason })?;
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
                self.interrupt(&Target::of(claimed), *cause, reason)?;
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
        let stage = Some(claimed.stage_id.as_str());
        self.append(
            &job.id,
            stage,
            EventPayload::StageStarted {
                attempt: claimed.attempt,
            },
        )?;
        let run = StageRun {
            repo: repo.clone(),
            relative,
            handoff_path: self.config.data_dir.join(&handoff_rel),
            handoff_rel,
            worktree,
        };
        let quota = fold_quota(&self.with_store(|s| s.quota_events())?);
        let required = stage_capabilities(&claimed.name);
        let now = self.env.now_ms();
        let routed = match route(
            self.routing,
            &RouteRequest {
                stage: &claimed.name,
                tier: DEFAULT_TIER,
                repo: job.repo.as_deref(),
                required: &required,
            },
            &quota,
            now,
        ) {
            Ok(routed) => routed,
            Err(e) => return Ok(Verdict::Fail(format!("routing: {e}"))),
        };
        let mut waits: Vec<i64> = routed
            .skipped
            .iter()
            .map(|(_, skip)| match skip {
                Skip::Exhausted { until_ms } => *until_ms,
                Skip::Utilization { resets_at_ms } => {
                    resets_at_ms.unwrap_or(now + self.routing.default_backoff_ms)
                }
            })
            .collect();
        let mut note = stored_note(&dir, claimed.attempt);
        let mut last_error: Option<String> = None;
        for (position, candidate) in routed.candidates.iter().enumerate() {
            match self.try_candidate(claimed, &run, position, candidate, note.as_deref())? {
                Attempt::Settled(verdict) => return Ok(verdict),
                Attempt::Quota { until_ms, reason } => {
                    waits.push(until_ms);
                    self.fall_back(claimed, candidate, &reason)?;
                    note =
                        Some(self.interrupt(&Target::of(claimed), RetryCause::Quota, &reason)?);
                }
                Attempt::Provider(reason) => {
                    self.fall_back(claimed, candidate, &reason)?;
                    note = Some(format!(
                        "The previous agent stopped before finishing: {reason}"
                    ));
                    last_error = Some(reason);
                }
            }
        }
        Ok(match (waits.iter().min(), last_error) {
            (Some(until), _) => Verdict::Park {
                until_ms: (*until).max(self.env.now_ms() + MIN_PARK_MS),
                reason: "every candidate is out of quota".to_owned(),
            },
            (None, Some(reason)) => Verdict::Fail(reason),
            (None, None) => Verdict::Fail("no candidate could run".to_owned()),
        })
    }

    fn fall_back(&self, claimed: &Claimed, candidate: &Candidate, reason: &str) -> Result<()> {
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::AgentFellBack {
                agent: candidate.agent.clone(),
                reason: reason.to_owned(),
            },
        )
    }

    fn record_quota(
        &self,
        claimed: &Claimed,
        candidate: &Candidate,
        classified: &Classified,
    ) -> Result<Option<i64>> {
        let now = self.env.now_ms();
        let exhausted_by_failure = match &classified.failure {
            Some(Failure::QuotaExhausted { resets_at_ms, .. }) => Some(*resets_at_ms),
            _ => None,
        };
        let obs = classified.observation.as_ref();
        let signal = match (obs, exhausted_by_failure) {
            (Some(o), _) if o.status == QuotaStatus::Exhausted => {
                Some((o.status, o.utilization_permille, o.resets_at_ms))
            }
            (o, Some(reset)) => Some((
                QuotaStatus::Exhausted,
                o.and_then(|o| o.utilization_permille),
                reset.or_else(|| o.and_then(|o| o.resets_at_ms)),
            )),
            (Some(o), None) => Some((o.status, o.utilization_permille, o.resets_at_ms)),
            (None, None) => None,
        };
        let Some((status, utilization_permille, reset)) = signal else {
            return Ok(None);
        };
        let resets_at_ms = match (status, reset) {
            (QuotaStatus::Exhausted, None) => Some(now + self.routing.default_backoff_ms),
            (_, known) => known,
        };
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::QuotaSignal {
                agent: candidate.agent.clone(),
                model: candidate.model.clone(),
                status,
                utilization_permille,
                resets_at_ms,
                reset_known: reset.is_some(),
            },
        )?;
        Ok((status == QuotaStatus::Exhausted)
            .then_some(resets_at_ms)
            .flatten())
    }

    fn try_candidate(
        &self,
        claimed: &Claimed,
        run: &StageRun,
        position: usize,
        candidate: &Candidate,
        note: Option<&str>,
    ) -> Result<Attempt> {
        let job = &claimed.job;
        let stage = Some(claimed.stage_id.as_str());
        let Some((kind, agent)) = self.fleet.get(&candidate.agent) else {
            return Ok(Attempt::Settled(Verdict::Fail(format!(
                "agent '{}' is not configured",
                candidate.agent
            ))));
        };
        if run.handoff_path.exists() {
            std::fs::remove_file(&run.handoff_path)?;
        }
        let Some(rendered) = self.render_prompt(claimed, run)? else {
            return Ok(Attempt::Settled(Verdict::Fail(format!(
                "no prompt template for stage '{}'",
                claimed.name
            ))));
        };
        let rendered = rendered.with_interruption_note(note);
        self.append(
            &job.id,
            stage,
            EventPayload::AgentInvoked {
                agent: candidate.agent.clone(),
                template: rendered.template.clone(),
                version: rendered.version,
                prompt_sha256: rendered.prompt_sha256.clone(),
            },
        )?;
        let egress_rel = run.relative.join("egress-requests.log");
        let egress_log = self.config.data_dir.join(&egress_rel);
        if egress_log.exists() {
            std::fs::remove_file(&egress_log)?;
        }
        let sandbox = self.config.trust.profile_for(
            &run.repo.to_string_lossy(),
            &[
                run.worktree.to_string_lossy().into_owned(),
                self.config
                    .data_dir
                    .join(&run.relative)
                    .to_string_lossy()
                    .into_owned(),
            ],
        );
        let invocation = Invocation {
            prompt: rendered.text,
            model: candidate.model.clone(),
            cwd: run.worktree.clone(),
            handoff_path: run.handoff_path.clone(),
            timeout: self
                .config
                .timeout
                .unwrap_or_else(|| stage_timeout(&claimed.name, tier_of(&claimed.name))),
            grace: self.config.kill_grace,
            sandbox: sandbox.clone(),
            egress_log: egress_log.clone(),
        };
        let started = self.env.now_ms();
        let stop = AtomicBool::new(false);
        let output = std::thread::scope(|scope| {
            let beat = scope.spawn(|| self.beat(&claimed.stage_id, &stop));
            let result = agent.run(&invocation, &mut |pid| {
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
        let requested = std::fs::read_to_string(&egress_log).unwrap_or_default();
        let blocked = blocked_hosts(&sandbox.egress_allowlist, &requested);
        for host in &blocked {
            self.append(
                &job.id,
                stage,
                EventPayload::EgressBlocked { host: host.clone() },
            )?;
        }
        let output = match output {
            Ok(output) => {
                self.audit(claimed, &sandbox, blocked)?;
                output
            }
            Err(e) => {
                self.audit(claimed, &sandbox, blocked)?;
                return Ok(Attempt::Settled(Verdict::Fail(format!("agent: {e:#}"))));
            }
        };
        let wall_ms = u64::try_from(self.env.now_ms() - started).ok();
        let suffix = if position == 0 {
            String::new()
        } else {
            format!("-{position}")
        };
        let raw_rel = run.relative.join(format!("raw-output{suffix}.jsonl"));
        std::fs::write(self.config.data_dir.join(&raw_rel), &output.stdout)?;
        self.record_artifact(&job.id, &claimed.stage_id, "raw-output", &raw_rel)?;
        if !output.stderr.is_empty() {
            let err_rel = run.relative.join(format!("raw-stderr{suffix}.txt"));
            std::fs::write(self.config.data_dir.join(&err_rel), &output.stderr)?;
            self.record_artifact(&job.id, &claimed.stage_id, "raw-stderr", &err_rel)?;
        }
        let classified = classify(
            kind,
            &RawRun {
                exit_code: output.exit_code,
                stdout: &output.stdout,
                stderr: &output.stderr,
                timed_out: output.timed_out,
            },
            None,
        );
        let mut usage = classified.usage.clone();
        usage.wall_ms = wall_ms;
        usage.model = usage.model.or_else(|| candidate.model.clone());
        usage.agent = Some(candidate.agent.clone());
        usage.candidate_position = u32::try_from(position).ok();
        let actual_tokens = usage
            .input_tokens
            .zip(usage.output_tokens)
            .map(|(i, o)| i.saturating_add(o));
        self.append(&job.id, stage, EventPayload::AgentFinished { usage })?;
        let until = self.record_quota(claimed, candidate, &classified)?;
        if let Some(reason) =
            self.record_budget(claimed, &candidate.agent, actual_tokens, wall_ms)?
        {
            return Ok(Attempt::Settled(Verdict::NeedsHuman(reason)));
        }
        Ok(match classified.failure {
            None => Attempt::Settled(self.accept_output(claimed, run)?),
            Some(Failure::QuotaExhausted { reason, .. }) => Attempt::Quota {
                until_ms: until.unwrap_or(self.env.now_ms() + self.routing.default_backoff_ms),
                reason,
            },
            Some(Failure::ProviderError(reason)) => Attempt::Provider(reason),
            Some(Failure::Config(reason)) => Attempt::Settled(Verdict::Fail(reason)),
            Some(failure) => {
                Attempt::Settled(Verdict::Retry(retry_cause(&failure), failure.reason()))
            }
        })
    }

    fn record_budget(
        &self,
        claimed: &Claimed,
        agent: &str,
        actual_tokens: Option<u64>,
        actual_wall_ms: Option<u64>,
    ) -> Result<Option<String>> {
        let job = &claimed.job.id;
        let stage = Some(claimed.stage_id.as_str());
        let events = self.with_store(|s| s.read_timeline(job))?;
        let estimate = estimate_for(&events, &claimed.name).unwrap_or_else(|| {
            estimate_stage(
                &claimed.name,
                tier_for(&claimed.name),
                agent,
                &[],
                &Defaults::standard(),
            )
        });
        self.append(
            job,
            stage,
            EventPayload::StageBudgetRecorded {
                stage: claimed.name.clone(),
                tier: estimate.tier,
                agent: agent.to_owned(),
                estimate_tokens: estimate.tokens,
                estimate_wall_ms: estimate.wall_ms,
                actual_tokens,
                actual_wall_ms,
            },
        )?;
        let events = self.with_store(|s| s.read_timeline(job))?;
        let Some(f) = actual_overrun(&events, self.config.budget_cap_tokens) else {
            return Ok(None);
        };
        self.append(
            job,
            None,
            EventPayload::FeedbackRecorded {
                kind: f.kind,
                severity: f.severity,
                text: f.text.clone(),
                confidence: f.confidence,
                alternatives: f.alternatives,
                impact: f.impact,
            },
        )?;
        Ok(Some(f.text))
    }

    fn audit(
        &self,
        claimed: &Claimed,
        profile: &SandboxProfile,
        blocked_requests: Vec<String>,
    ) -> Result<()> {
        self.append(
            &claimed.job.id,
            Some(claimed.stage_id.as_str()),
            EventPayload::SandboxAudit {
                profile_hash: profile.hash(),
                mounts: profile.mounts.iter().map(Mount::describe).collect(),
                egress_allowlist: profile.egress_allowlist.clone(),
                blocked_requests,
                mcp_activations: Vec::new(),
                diff_policy_result: None,
                pushed_ref: None,
                pr_url: None,
            },
        )
    }

    fn templates(&self) -> Templates {
        PIPELINE_STAGES
            .iter()
            .fold(Templates::embedded(), |templates, name| {
                let text = std::fs::read_to_string(
                    self.config
                        .data_dir
                        .join("prompts")
                        .join(format!("{name}.md")),
                );
                match text {
                    Ok(text) => templates
                        .clone()
                        .with_override(name, &text)
                        .unwrap_or(templates),
                    Err(_) => templates,
                }
            })
    }

    fn read_data(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.config.data_dir.join(relative)).ok()
    }

    fn render_prompt(&self, claimed: &Claimed, run: &StageRun) -> Result<Option<Rendered>> {
        let events = self.with_store(|s| s.read_timeline(&claimed.job.id))?;
        let previous = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::HandoffWritten { path }
                if e.stage_id.as_deref() != Some(claimed.stage_id.as_str()) =>
            {
                self.read_data(path)
            }
            _ => None,
        });
        let report = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ArtifactRecorded { kind, path, .. } if kind == "review-report" => {
                self.read_data(path)
            }
            _ => None,
        });
        let artifact_dir = self.config.data_dir.join(&run.relative);
        Ok(render(
            &self.templates(),
            &Vars {
                job: claimed.job.id.as_str(),
                stage: &claimed.name,
                task: &claimed.job.prompt,
                handoff_path: &run.handoff_path.to_string_lossy(),
                artifact_dir: &artifact_dir.to_string_lossy(),
                spec_dir: &spec_dir(&claimed.job.id),
                previous_handoff: previous.as_deref(),
                review_report: report.as_deref(),
            },
        ))
    }

    fn record_feedback(
        &self,
        claimed: &Claimed,
        actor: Actor,
        entries: Vec<Feedback>,
    ) -> Result<()> {
        for f in entries {
            let event = NewEvent {
                id: self.env.new_ulid(),
                job_id: claimed.job.id.clone(),
                stage_id: Some(claimed.stage_id.clone()),
                ts_ms: self.env.now_ms(),
                actor,
                payload: EventPayload::FeedbackRecorded {
                    kind: f.kind,
                    severity: f.severity,
                    text: f.text,
                    confidence: f.confidence,
                    alternatives: f.alternatives,
                    impact: f.impact,
                },
            };
            self.with_store(|s| s.append_event(event))?;
        }
        Ok(())
    }

    fn accept_output(&self, claimed: &Claimed, run: &StageRun) -> Result<Verdict> {
        let handoff_rel = run.handoff_rel.as_path();
        let path = self.config.data_dir.join(handoff_rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                "agent wrote no handoff".to_owned(),
            ));
        };
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "handoff", handoff_rel)?;
        if let Err(e) = validate(&text) {
            return Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                format!("invalid handoff: {e}"),
            ));
        }
        let outcome = match claimed.name.as_str() {
            "grill" => self.accept_grill(claimed, run)?,
            "spec" => self.accept_spec(claimed, run)?,
            "review" => self.accept_review(claimed, run)?,
            _ => Ok(Vec::new()),
        };
        let extra = match outcome {
            Ok(extra) => extra,
            Err(reason) => return Ok(Verdict::Retry(RetryCause::InvalidHandoff, reason)),
        };
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::HandoffWritten {
                path: handoff_rel.to_string_lossy().into_owned(),
            },
        )?;
        self.record_feedback(claimed, Actor::Agent, parse_feedback(&text))?;
        self.record_feedback(claimed, Actor::Daemon, extra)?;
        if claimed.name == "review" {
            self.plan_review_follow_up(claimed, run)?;
        }
        Ok(Verdict::Succeeded)
    }

    fn accept_grill(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let rel = run.relative.join("grill.md");
        let Some(transcript) = self.read_data(&rel.to_string_lossy()) else {
            return Ok(Ok(Vec::new()));
        };
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "grill-transcript", &rel)?;
        Ok(Ok(
            grill::assess(&transcript, grill::DEFAULT_ROUND_CAP).feedback
        ))
    }

    fn accept_spec(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let dir = run.worktree.join(spec_dir(&claimed.job.id));
        let Ok(spec_text) = std::fs::read_to_string(dir.join("spec.md")) else {
            return Ok(Err("invalid spec: agent wrote no spec.md".to_owned()));
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("tickets"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "md"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let tickets: Vec<(String, String)> = files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some((name, std::fs::read_to_string(p).ok()?))
            })
            .collect();
        if let Err(e) = spec::validate(&spec_text, &tickets) {
            return Ok(Err(format!("invalid spec: {e}")));
        }
        let worktree_rel = run
            .worktree
            .strip_prefix(&self.config.data_dir)
            .unwrap_or(&run.worktree)
            .join(spec_dir(&claimed.job.id));
        self.record_artifact(
            &claimed.job.id,
            &claimed.stage_id,
            "spec",
            &worktree_rel.join("spec.md"),
        )?;
        for file in &files {
            if let Some(name) = file.file_name() {
                self.record_artifact(
                    &claimed.job.id,
                    &claimed.stage_id,
                    "ticket",
                    &worktree_rel.join("tickets").join(name),
                )?;
            }
        }
        Ok(Ok(Vec::new()))
    }

    fn accept_review(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let rel = run.relative.join("review.md");
        let Some(report) = self.read_data(&rel.to_string_lossy()) else {
            return Ok(Err("invalid review: agent wrote no review.md".to_owned()));
        };
        if let Err(e) = review::parse_report(&report) {
            return Ok(Err(format!("invalid review: {e}")));
        }
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "review-report", &rel)?;
        Ok(Ok(Vec::new()))
    }

    fn plan_review_follow_up(&self, claimed: &Claimed, run: &StageRun) -> Result<()> {
        let report = self
            .read_data(&run.relative.join("review.md").to_string_lossy())
            .unwrap_or_default();
        let findings = review::parse_report(&report).unwrap_or_default();
        let events = self.with_store(|s| s.read_timeline(&claimed.job.id))?;
        let added = vec![
            PlannedStage {
                name: "fix".to_owned(),
                reason: "fix the blocking review findings".to_owned(),
            },
            PlannedStage {
                name: "review".to_owned(),
                reason: "re-review after the fix".to_owned(),
            },
        ];
        let can_plan = amend_plan(
            &current_plan(&events),
            "review",
            added.clone(),
            DEFAULT_MAX_STAGES,
        )
        .is_ok();
        match review::decide(&findings, fix_rounds(&events), can_plan) {
            Decision::Clean => Ok(()),
            Decision::Fix => self.append(
                &claimed.job.id,
                None,
                EventPayload::PlanAmended {
                    after: "review".to_owned(),
                    stages: added,
                },
            ),
            Decision::Leftover(entries) => self.record_feedback(claimed, Actor::Daemon, entries),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::AtomicI64;
    use std::sync::Arc;

    use pizza_delivery_core::budget::DEFAULT_CAP_TOKENS;
    use pizza_delivery_core::event::Event;
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
            let requests = *self.egress.lock().map_err(|_| anyhow!("poisoned"))?;
            if let Some(requests) = requests {
                std::fs::write(&inv.egress_log, requests)?;
            }
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
            std::fs::write(
                inv.handoff_path
                    .with_file_name(format!("prompt-{}.txt", self.tag)),
                &inv.prompt,
            )?;
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
                    require_triage: false,
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
            *self.egress.lock().map_err(|_| anyhow!("poisoned"))? = Some(requests);
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

        fn with_store<T>(
            &self,
            f: impl FnOnce(&mut dyn Store) -> Result<T, StoreError>,
        ) -> Result<T> {
            let mut guard = self.store.lock().map_err(|_| anyhow!("poisoned"))?;
            Ok(f(guard.as_mut())?)
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

        fn prompts(&self) -> Result<Vec<String>> {
            Ok(self
                .prompts
                .lock()
                .map_err(|_| anyhow!("poisoned"))?
                .clone())
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
                "sandbox-audit",
                "artifact-recorded",
                "agent-finished",
                "stage-budget-recorded",
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
    fn every_invocation_records_an_audit_entry_with_all_fields() -> Result<()> {
        let rig = Rig::new(vec![good()])?;
        rig.worker().tick()?;

        let audits = rig.audits()?;
        assert_eq!(audits.len(), 1);
        let EventPayload::SandboxAudit {
            profile_hash,
            mounts,
            egress_allowlist,
            blocked_requests,
            diff_policy_result,
            pushed_ref,
            pr_url,
            ..
        } = &audits[0]
        else {
            return Err(anyhow!("not an audit"));
        };
        assert_eq!(profile_hash.len(), 64);
        assert!(mounts
            .iter()
            .any(|m| m.ends_with(":rw") && m.contains("J1")));
        assert_eq!(egress_allowlist, &vec!["api.anthropic.com".to_owned()]);
        assert!(blocked_requests.is_empty());
        assert_eq!(
            (diff_policy_result, pushed_ref, pr_url),
            (&None, &None, &None)
        );
        Ok(())
    }

    #[test]
    fn a_failed_agent_run_is_audited_too() -> Result<()> {
        let rig = Rig::new(vec![])?;
        rig.worker().tick()?;
        assert_eq!(rig.audits()?.len(), 1);
        Ok(())
    }

    #[test]
    fn blocked_egress_is_ledgered_per_host_and_in_the_audit() -> Result<()> {
        let rig = Rig::new(vec![good()])?
            .with_egress("api.anthropic.com\nevil.example\nevil.example\ntracker.io\n")?;
        rig.worker().tick()?;

        let blocked: Vec<String> = rig
            .timeline()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::EgressBlocked { host } => Some(host),
                _ => None,
            })
            .collect();
        assert_eq!(blocked, vec!["evil.example", "tracker.io"]);
        let audits = rig.audits()?;
        assert!(matches!(
            &audits[0],
            EventPayload::SandboxAudit { blocked_requests, .. }
                if blocked_requests == &vec!["evil.example".to_owned(), "tracker.io".to_owned()]
        ));
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
            exit_code: 1,
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
            require_triage: false,
            agent: "claude".into(),
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
            require_triage: false,
            agent: "claude".into(),
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

    const OPENCODE_OWNER: &str = r#"
[agents.opencode]
adapter = "opencode"
command = "opencode"
[agents.hermes]
adapter = "hermes"
command = "hermes"

[[rules]]
chain = [{ agent = "claude" }, { agent = "opencode" }]
"#;

    const QUOTA_429: &str = "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429}\n";
    const QUOTA_WITH_RESET: &str = "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\",\"resetsAt\":10}}\n{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429}\n";

    fn exhausted(stdout: &'static str) -> Script {
        Script {
            handoff: None,
            stdout,
            exit_code: 1,
            ..Script::default()
        }
    }

    fn invoked(rig: &Rig) -> Result<Vec<String>> {
        Ok(rig
            .timeline()?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::AgentInvoked { agent, .. } => Some(agent),
                _ => None,
            })
            .collect())
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
    fn quota_exhaustion_falls_back_to_the_next_candidate_with_an_interruption_note() -> Result<()> {
        let rig = Rig::with_chain(
            vec![
                ("claude", AdapterKind::Claude, vec![exhausted(QUOTA_429)]),
                ("opencode", AdapterKind::Opencode, vec![good()]),
            ],
            OPENCODE_OWNER,
        )?;

        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::Completed);
        assert_eq!(invoked(&rig)?, ["claude", "opencode"]);
        let kinds = rig.kinds()?;
        assert!(kinds.contains(&"quota-signal"));
        assert!(kinds.contains(&"agent-fell-back"));
        assert!(!kinds.contains(&"stage-retried"));
        let stage_dir = rig._dir.path().join("jobs/J1/stages/1-implement");
        let first = std::fs::read_to_string(stage_dir.join("prompt-claude.txt"))?;
        let second = std::fs::read_to_string(stage_dir.join("prompt-opencode.txt"))?;
        assert!(!first.contains("Interruption note"));
        assert!(second.contains("Interruption note"));
        Ok(())
    }

    #[test]
    fn a_fully_exhausted_chain_parks_the_job_until_the_earliest_known_reset_and_resumes_on_its_own(
    ) -> Result<()> {
        let rig = Rig::with_chain(
            vec![
                (
                    "claude",
                    AdapterKind::Claude,
                    vec![exhausted(QUOTA_WITH_RESET), good()],
                ),
                (
                    "opencode",
                    AdapterKind::Opencode,
                    vec![exhausted(
                        "{\"type\":\"error\",\"error\":{\"message\":\"quota exceeded\"}}\n",
                    )],
                ),
            ],
            OPENCODE_OWNER,
        )?;

        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::WaitingForQuota);
        let parked = rig.timeline()?.into_iter().find_map(|e| match e.payload {
            EventPayload::JobParked { until_ms, .. } => Some(until_ms),
            _ => None,
        });
        assert_eq!(parked, Some(10_000));
        assert!(!rig.worker().tick()?);
        rig.env.now.store(9_999, Ordering::SeqCst);
        assert!(!rig.worker().tick()?);

        rig.env.now.store(10_000, Ordering::SeqCst);
        assert!(rig.worker().tick()?);

        assert_eq!(rig.job_state()?, JobState::Completed);
        let attempts: Vec<u32> = fold_stages(&rig.timeline()?)
            .iter()
            .map(|s| s.attempt)
            .collect();
        assert_eq!(attempts, [1]);
        Ok(())
    }

    #[test]
    fn exhaustion_without_a_known_reset_parks_for_the_default_backoff() -> Result<()> {
        let rig = Rig::new(vec![exhausted(QUOTA_429)])?;

        rig.worker().tick()?;

        let timeline = rig.timeline()?;
        let signal = timeline.iter().find_map(|e| match &e.payload {
            EventPayload::QuotaSignal {
                reset_known,
                resets_at_ms,
                ..
            } => Some((*reset_known, *resets_at_ms)),
            _ => None,
        });
        assert_eq!(signal, Some((false, Some(100 + 3_600_000))));
        assert_eq!(rig.job_state()?, JobState::WaitingForQuota);
        Ok(())
    }

    #[test]
    fn a_candidate_known_to_be_exhausted_is_skipped_without_being_run() -> Result<()> {
        let rig = Rig::with_chain(
            vec![
                (
                    "claude",
                    AdapterKind::Claude,
                    vec![exhausted(QUOTA_WITH_RESET)],
                ),
                ("opencode", AdapterKind::Opencode, vec![good(), good()]),
            ],
            OPENCODE_OWNER,
        )?;
        rig.worker().tick()?;
        rig.with_store(|s| {
            s.enqueue_job(NewJob {
                id: JobId::new("J2"),
                event_id: "E2".into(),
                prompt: "second".into(),
                repo: Some("/src/my repo".into()),
                priority: Priority::Normal,
                submitted_at_ms: 0,
            })
            .map(|_| ())
        })?;
        rig.env.now.store(5_000, Ordering::SeqCst);

        rig.worker().tick()?;

        let second: Vec<String> = rig
            .with_store(|s| s.read_timeline(&JobId::new("J2")))?
            .into_iter()
            .filter_map(|e| match e.payload {
                EventPayload::AgentInvoked { agent, .. } => Some(agent),
                _ => None,
            })
            .collect();
        assert_eq!(second, ["opencode"]);
        Ok(())
    }

    #[test]
    fn a_configuration_error_fails_fast_without_retry_or_fallback() -> Result<()> {
        let rig = Rig::with_chain(
            vec![
                (
                    "hermes",
                    AdapterKind::Hermes,
                    vec![Script {
                        handoff: None,
                        stdout: "",
                        stderr: "No usable credentials found for provider 'gemini'.",
                        exit_code: 1,
                        timed_out: false,
                    }],
                ),
                ("claude", AdapterKind::Claude, vec![good()]),
            ],
            "[[rules]]\nchain = [{ agent = \"hermes\" }, { agent = \"claude\" }]\n[agents.hermes]\nadapter = \"hermes\"\ncommand = \"hermes\"\n",
        )?;

        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::Failed);
        assert_eq!(invoked(&rig)?, ["hermes"]);
        let kinds = rig.kinds()?;
        assert!(!kinds.contains(&"stage-retried"));
        assert!(!kinds.contains(&"agent-fell-back"));
        Ok(())
    }

    #[test]
    fn terminal_provider_errors_fall_back_and_fail_the_job_when_the_chain_runs_out() -> Result<()> {
        let rig = Rig::with_chain(
            vec![
                ("claude", AdapterKind::Claude, vec![exhausted(ERR_RESULT)]),
                (
                    "opencode",
                    AdapterKind::Opencode,
                    vec![exhausted(
                        "{\"type\":\"error\",\"error\":{\"type\":\"provider.no-route\"}}\n",
                    )],
                ),
            ],
            OPENCODE_OWNER,
        )?;

        rig.worker().tick()?;

        assert_eq!(rig.job_state()?, JobState::Failed);
        assert_eq!(invoked(&rig)?, ["claude", "opencode"]);
        assert!(!rig.kinds()?.contains(&"job-parked"));
        Ok(())
    }

    #[test]
    fn a_healthy_rate_limit_event_is_ledgered_as_an_allowed_quota_signal() -> Result<()> {
        let rig = Rig::new(vec![Script {
            handoff: Some(HANDOFF),
            stdout: "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\",\"resetsAt\":50,\"unifiedWindows\":{\"five_hour\":{\"utilization\":0.95,\"resetsAt\":50}}}}\n{\"type\":\"result\",\"is_error\":false}\n",
            stderr: "",
            exit_code: 0,
            timed_out: false,
        }])?;

        rig.worker().tick()?;

        let signal = rig.timeline()?.into_iter().find_map(|e| match e.payload {
            EventPayload::QuotaSignal {
                status,
                utilization_permille,
                ..
            } => Some((status, utilization_permille)),
            _ => None,
        });
        assert_eq!(signal, Some((QuotaStatus::Allowed, Some(950))));
        assert_eq!(rig.job_state()?, JobState::Completed);
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

        let prompts = rig.prompts()?;
        assert!(!prompts[0].contains("Interruption note"));
        assert!(prompts[1].contains("Interruption note"));
        assert!(prompts[1].contains("wip1"));
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
            require_triage: false,
            agent: "claude".into(),
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
        let prompts = rig.prompts()?;
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
            exit_code: 1,
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
