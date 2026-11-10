use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::job::JobState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetryCause {
    #[default]
    Transient,
    Timeout,
    Quota,
    InvalidHandoff,
    Crash,
}

impl RetryCause {
    pub fn as_str(self) -> &'static str {
        match self {
            RetryCause::Transient => "transient",
            RetryCause::Timeout => "timeout",
            RetryCause::Quota => "quota",
            RetryCause::InvalidHandoff => "invalid-handoff",
            RetryCause::Crash => "crash",
        }
    }

    fn limit(self) -> usize {
        match self {
            RetryCause::Timeout | RetryCause::InvalidHandoff | RetryCause::Crash => 1,
            RetryCause::Transient | RetryCause::Quota => 2,
        }
    }

    pub fn interrupts_work(self) -> bool {
        matches!(
            self,
            RetryCause::Timeout | RetryCause::Quota | RetryCause::Crash
        )
    }
}

pub const TOTAL_RETRY_CAP: usize = 4;

pub fn may_retry(cause: RetryCause, history: &[RetryCause]) -> bool {
    history.len() < TOTAL_RETRY_CAP
        && history.iter().filter(|c| **c == cause).count() < cause.limit()
}

pub fn retry_history(events: &[Event], stage_id: &str) -> Vec<RetryCause> {
    events
        .iter()
        .filter(|e| e.stage_id.as_deref() == Some(stage_id))
        .filter_map(|e| match &e.payload {
            EventPayload::StageRetried { cause, .. } => Some(*cause),
            _ => None,
        })
        .collect()
}

pub fn classify_failure(reason: &str) -> RetryCause {
    let lowered = reason.to_lowercase();
    if ["quota", "usage limit", "rate limit", "rate_limit"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        RetryCause::Quota
    } else {
        RetryCause::Transient
    }
}

pub fn is_cancelled(events: &[Event]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::JobCancelled {}))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Small,
    Medium,
    Large,
}

pub fn base_timeout(stage: &str) -> Duration {
    let minutes = match stage {
        "triage" => 5,
        "implement" => 60,
        _ => 20,
    };
    Duration::from_secs(minutes * 60)
}

pub fn stage_timeout(stage: &str, tier: Tier) -> Duration {
    let base = base_timeout(stage);
    match tier {
        Tier::Small => base / 2,
        Tier::Medium => base,
        Tier::Large => base * 2,
    }
}

pub const DEFAULT_RETENTION_MS: i64 = 14 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorktreeFacts {
    pub job_state: JobState,
    pub finished_at_ms: Option<i64>,
    pub pushed: bool,
    pub dirty: bool,
}

pub fn worktree_removable(facts: WorktreeFacts, now_ms: i64, retention_ms: i64) -> bool {
    facts.job_state == JobState::Completed
        && facts.pushed
        && !facts.dirty
        && facts
            .finished_at_ms
            .is_some_and(|finished| now_ms - finished >= retention_ms)
}

pub struct Interruption<'a> {
    pub cause: RetryCause,
    pub stage: &'a str,
    pub attempt: u32,
    pub detail: &'a str,
    pub wip_commit: Option<&'a str>,
}

pub fn render_interruption_note(i: &Interruption<'_>) -> String {
    let saved = match i.wip_commit {
        Some(sha) => {
            format!(
                "Uncommitted work was saved in WIP commit {sha}. Review it and continue from it."
            )
        }
        None => "There was no uncommitted work to save.".to_owned(),
    };
    format!(
        "# Interruption note\n\nAttempt {} of stage {} was interrupted ({}): {}\n\n{saved}\nCheck the worktree state with git status and git log before redoing anything.\n",
        i.attempt,
        i.stage,
        i.cause.as_str(),
        i.detail
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timeout_is_retried_once() {
        assert!(may_retry(RetryCause::Timeout, &[]));
        assert!(!may_retry(RetryCause::Timeout, &[RetryCause::Timeout]));
    }

    #[test]
    fn transient_failures_are_retried_up_to_twice() {
        let once = [RetryCause::Transient];
        let twice = [RetryCause::Transient, RetryCause::Transient];
        assert!(may_retry(RetryCause::Transient, &once));
        assert!(!may_retry(RetryCause::Transient, &twice));
    }

    #[test]
    fn no_more_than_the_total_cap_of_retries_ever_happen() {
        let spent = [
            RetryCause::Timeout,
            RetryCause::Transient,
            RetryCause::Transient,
            RetryCause::Crash,
        ];
        assert!(!may_retry(RetryCause::Quota, &spent));
    }

    #[test]
    fn quota_messages_are_told_apart_from_other_transient_failures() {
        assert_eq!(
            classify_failure("Claude usage limit reached"),
            RetryCause::Quota
        );
        assert_eq!(classify_failure("HTTP 429 rate limit"), RetryCause::Quota);
        assert_eq!(classify_failure("connection reset"), RetryCause::Transient);
    }

    #[test]
    fn timeouts_scale_with_the_budget_tier() {
        assert_eq!(
            stage_timeout("implement", Tier::Medium),
            Duration::from_secs(3600)
        );
        assert_eq!(
            stage_timeout("implement", Tier::Small),
            Duration::from_secs(1800)
        );
        assert_eq!(
            stage_timeout("triage", Tier::Large),
            Duration::from_secs(600)
        );
    }

    #[test]
    fn only_a_completed_pushed_clean_worktree_past_retention_is_removable() {
        let day = 86_400_000;
        let base = WorktreeFacts {
            job_state: JobState::Completed,
            finished_at_ms: Some(0),
            pushed: true,
            dirty: false,
        };
        assert!(worktree_removable(base, 15 * day, 14 * day));
        assert!(!worktree_removable(base, 13 * day, 14 * day));
        let dirty = WorktreeFacts {
            dirty: true,
            ..base
        };
        assert!(!worktree_removable(dirty, 99 * day, 14 * day));
        let unpushed = WorktreeFacts {
            pushed: false,
            ..base
        };
        assert!(!worktree_removable(unpushed, 99 * day, 14 * day));
        for state in [
            JobState::Failed,
            JobState::Cancelled,
            JobState::Running,
            JobState::Queued,
        ] {
            let other = WorktreeFacts {
                job_state: state,
                ..base
            };
            assert!(!worktree_removable(other, 99 * day, 14 * day));
        }
    }

    #[test]
    fn the_note_names_the_cause_the_attempt_and_the_wip_commit() {
        let note = render_interruption_note(&Interruption {
            cause: RetryCause::Timeout,
            stage: "implement",
            attempt: 1,
            detail: "agent timed out",
            wip_commit: Some("abc123"),
        });
        assert!(note.contains("Attempt 1 of stage implement"));
        assert!(note.contains("timeout"));
        assert!(note.contains("abc123"));
    }
}
