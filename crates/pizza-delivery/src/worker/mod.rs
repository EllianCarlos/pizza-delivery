use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Result};
use pizza_delivery_core::budget::{
    actual_overrun, estimate_for, estimate_stage, tier_for, Defaults, SizeTier,
};
use pizza_delivery_core::classify::{classify, Classified, Failure, RawRun};
use pizza_delivery_core::event::{Actor, Event, EventPayload};
use pizza_delivery_core::feedback::parse_feedback;
use pizza_delivery_core::gate::{gate, GateDecision, GateItem, GateThresholds, Impact};
use pizza_delivery_core::grill;
use pizza_delivery_core::handoff::validate;
use pizza_delivery_core::memorize::{
    contradiction_risk, decide, engram_content, engram_topic_key, job_tag, mnemon_content,
    mnemon_tags, parse_engram_saved, parse_engram_search, parse_mnemon_recall, parse_mnemon_saved,
    parse_plan, prompt as memorize_prompt, should_memorize, Decision as MemoryDecision, Entry,
    Existing, Store as MemoryStore, ARTIFACT_KIND,
};
use pizza_delivery_core::prompt::{render, Rendered, Templates, Vars};
use pizza_delivery_core::quota::{fold_quota, QuotaStatus};
use pizza_delivery_core::review::{self, Decision};
use pizza_delivery_core::routing::{
    route, stage_capabilities, Candidate, RouteRequest, RoutingConfig, Skip,
};
use pizza_delivery_core::sandbox::{blocked_hosts, Mount, SandboxProfile, TrustConfig};
use pizza_delivery_core::ship::{pr_title, render_pr_body, DiffPolicy};
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
use crate::git::{Git, MergeReadiness};
use crate::handler::Env;
use crate::lock::acquire;
use crate::process::{kill_group, start_ticks, terminate_group};
use pizza_delivery_core::mcp::McpEvent;

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
    pub budget_defaults: Defaults,
    pub gate: GateThresholds,
    pub require_triage: bool,
    pub mcp: Option<McpSetup>,
    pub memory: Option<MemorySetup>,
    pub auto_merge: bool,
}

pub struct MemorySetup {
    pub engram: PathBuf,
    pub mnemon: PathBuf,
}

pub struct McpSetup {
    pub broker: PathBuf,
    pub catalog: PathBuf,
    pub secrets: PathBuf,
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
        let mut guard = acquire(self.store, "store")?;
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

    fn parent_branch(&self, job: &JobId) -> Result<Option<String>> {
        let events = self.with_store(|s| s.read_timeline(job))?;
        Ok(events.iter().find_map(|e| match &e.payload {
            EventPayload::JobSubmitted {
                follow_up: Some(follow_up),
                ..
            } => Some(format!("pizza/{}", follow_up.parent)),
            _ => None,
        }))
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
                    match self.deliver(job, &events)? {
                        Ok(()) => self.append(job, None, EventPayload::JobCompleted {})?,
                        Err(reason) => {
                            self.append(job, None, EventPayload::JobFailed { reason })?;
                        }
                    }
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
        self.memorize_if_due(job);
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
}

mod accept;
mod memorize;
mod prompt;
mod record;
mod ship;
mod stage;

#[cfg(test)]
mod tests;
