use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::id::JobId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum JobState {
    Queued,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: JobId,
    pub state: JobState,
    pub prompt: String,
    pub submitted_at_ms: i64,
    pub last_seq: u64,
}

pub fn apply(state: Option<Job>, event: &Event) -> Option<Job> {
    match (&event.payload, state) {
        (EventPayload::JobSubmitted { prompt }, None) => Some(Job {
            id: event.job_id.clone(),
            state: JobState::Queued,
            prompt: prompt.clone(),
            submitted_at_ms: event.ts_ms,
            last_seq: event.seq,
        }),
        (_, Some(job)) => Some(Job {
            last_seq: event.seq,
            ..job
        }),
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
            },
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
                submitted_at_ms: 42,
                last_seq: 1
            })
        );
    }

    #[test]
    fn empty_history_has_no_job() {
        assert_eq!(fold(&[]), None);
    }
}
