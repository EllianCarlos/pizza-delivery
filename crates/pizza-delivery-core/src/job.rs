use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::id::JobId;
use crate::schedule::Priority;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Completed => "completed",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub state: JobState,
    pub prompt: String,
    pub repo: Option<String>,
    pub priority: Priority,
    pub submitted_at_ms: i64,
    pub last_seq: u64,
}

fn next_state(current: JobState, payload: &EventPayload) -> JobState {
    if current == JobState::Cancelled {
        return current;
    }
    match payload {
        EventPayload::JobCancelled {} => JobState::Cancelled,
        EventPayload::StageClaimed { .. } => JobState::Running,
        EventPayload::StageRetried { .. } => JobState::Queued,
        EventPayload::JobCompleted {} => JobState::Completed,
        EventPayload::JobFailed { .. } => JobState::Failed,
        _ => current,
    }
}

pub fn apply(state: Option<Job>, event: &Event) -> Option<Job> {
    match (&event.payload, state) {
        (
            EventPayload::JobSubmitted {
                prompt,
                repo,
                priority,
            },
            None,
        ) => Some(Job {
            id: event.job_id.clone(),
            state: JobState::Queued,
            prompt: prompt.clone(),
            repo: repo.clone(),
            priority: *priority,
            submitted_at_ms: event.ts_ms,
            last_seq: event.seq,
        }),
        (payload, Some(job)) => Some(Job {
            state: next_state(job.state, payload),
            last_seq: event.seq,
            ..job
        }),
        (_, None) => None,
    }
}

pub fn fold<'a>(events: impl IntoIterator<Item = &'a Event>) -> Option<Job> {
    events.into_iter().fold(None, apply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;

    fn submitted(seq: u64) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: JobId::new("J1"),
            stage_id: None,
            seq,
            ts_ms: 42,
            actor: Actor::Human,
            payload: EventPayload::JobSubmitted {
                prompt: "do it".into(),
                repo: Some("/r".into()),
                priority: Priority::High,
            },
        }
    }

    #[test]
    fn a_cancelled_job_stays_cancelled_whatever_the_dying_stage_reports() {
        let history = [
            submitted(1),
            after(2, EventPayload::JobCancelled {}),
            after(3, EventPayload::JobFailed { reason: "x".into() }),
        ];
        assert_eq!(fold(&history).map(|j| j.state), Some(JobState::Cancelled));
    }

    #[test]
    fn the_priority_of_the_submission_is_kept_on_the_job() {
        assert_eq!(
            fold(&[submitted(1)]).map(|j| j.priority),
            Some(Priority::High)
        );
    }

    fn after(seq: u64, payload: EventPayload) -> Event {
        Event {
            payload,
            ..submitted(seq)
        }
    }

    #[test]
    fn submission_folds_into_a_queued_job() {
        let job = fold(&[submitted(1)]);
        assert_eq!(
            job,
            Some(Job {
                id: JobId::new("J1"),
                state: JobState::Queued,
                prompt: "do it".into(),
                repo: Some("/r".into()),
                priority: Priority::High,
                submitted_at_ms: 42,
                last_seq: 1
            })
        );
    }

    #[test]
    fn claim_runs_retry_requeues_and_completion_finishes_the_job() {
        let claimed = EventPayload::StageClaimed {
            holder: "w".into(),
            attempt: 1,
            lease_expires_ms: 9,
        };
        let retried = EventPayload::StageRetried {
            reason: "r".into(),
            cause: Default::default(),
        };
        let history = [
            submitted(1),
            after(2, claimed.clone()),
            after(3, retried),
            after(4, claimed),
            after(5, EventPayload::JobCompleted {}),
        ];
        let states: Vec<JobState> = (1..=history.len())
            .filter_map(|n| fold(&history[..n]).map(|j| j.state))
            .skip(1)
            .collect();
        assert_eq!(
            states,
            vec![
                JobState::Running,
                JobState::Queued,
                JobState::Running,
                JobState::Completed
            ]
        );
    }

    #[test]
    fn empty_history_has_no_job() {
        assert_eq!(fold(&[]), None);
    }
}
