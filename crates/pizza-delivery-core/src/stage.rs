use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::usage::Usage;

pub const MAX_ATTEMPTS: u32 = 2;
pub const STAGE_NAME: &str = "implement";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum StageState {
    Queued,
    Running,
    Succeeded,
    Failed,
}

impl StageState {
    pub fn as_str(self) -> &'static str {
        match self {
            StageState::Queued => "queued",
            StageState::Running => "running",
            StageState::Succeeded => "succeeded",
            StageState::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageView {
    pub id: String,
    pub n: u32,
    pub name: String,
    pub state: StageState,
    pub attempt: u32,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimPlan {
    pub existing_stage_id: Option<String>,
    pub n: u32,
    pub name: String,
    pub attempt: u32,
}

pub fn fold_stages(events: &[Event]) -> Vec<StageView> {
    events.iter().fold(Vec::new(), |mut stages, event| {
        let Some(stage_id) = event.stage_id.as_deref() else {
            return stages;
        };
        if let EventPayload::StagePlanned { n, name } = &event.payload {
            stages.push(StageView {
                id: stage_id.to_owned(),
                n: *n,
                name: name.clone(),
                state: StageState::Queued,
                attempt: 0,
                usage: None,
            });
            return stages;
        }
        if let Some(stage) = stages.iter_mut().find(|s| s.id == stage_id) {
            match &event.payload {
                EventPayload::StageClaimed { attempt, .. } => {
                    stage.state = StageState::Running;
                    stage.attempt = *attempt;
                }
                EventPayload::StageRetried { .. } => stage.state = StageState::Queued,
                EventPayload::StageSucceeded {} => stage.state = StageState::Succeeded,
                EventPayload::StageFailed { .. } => stage.state = StageState::Failed,
                EventPayload::AgentFinished { usage } => stage.usage = Some(usage.clone()),
                _ => {}
            }
        }
        stages
    })
}

pub fn next_claim(events: &[Event]) -> Option<ClaimPlan> {
    let finished = events.iter().any(|e| {
        matches!(
            e.payload,
            EventPayload::JobCompleted {}
                | EventPayload::JobFailed { .. }
                | EventPayload::JobNeedsHuman { .. }
        )
    });
    if finished {
        return None;
    }
    match fold_stages(events).into_iter().last() {
        None => Some(ClaimPlan {
            existing_stage_id: None,
            n: 1,
            name: STAGE_NAME.to_owned(),
            attempt: 1,
        }),
        Some(stage) if stage.state == StageState::Queued => Some(ClaimPlan {
            existing_stage_id: Some(stage.id),
            n: stage.n,
            name: stage.name,
            attempt: stage.attempt + 1,
        }),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::id::JobId;

    fn ev(seq: u64, stage: Option<&str>, payload: EventPayload) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: JobId::new("J1"),
            stage_id: stage.map(str::to_owned),
            seq,
            ts_ms: 0,
            actor: Actor::Daemon,
            payload,
        }
    }

    fn submitted() -> Event {
        ev(
            1,
            None,
            EventPayload::JobSubmitted {
                prompt: "p".into(),
                repo: Some("/r".into()),
            },
        )
    }

    fn planned() -> Event {
        ev(
            2,
            Some("S1"),
            EventPayload::StagePlanned {
                n: 1,
                name: "implement".into(),
            },
        )
    }

    fn claimed(seq: u64, attempt: u32) -> Event {
        ev(
            seq,
            Some("S1"),
            EventPayload::StageClaimed {
                holder: "w".into(),
                attempt,
                lease_expires_ms: 10,
            },
        )
    }

    #[test]
    fn a_fresh_job_plans_stage_one_attempt_one() {
        assert_eq!(
            next_claim(&[submitted()]),
            Some(ClaimPlan {
                existing_stage_id: None,
                n: 1,
                name: "implement".into(),
                attempt: 1
            })
        );
    }

    #[test]
    fn a_running_stage_is_not_claimable() {
        assert_eq!(next_claim(&[submitted(), planned(), claimed(3, 1)]), None);
    }

    #[test]
    fn a_retried_stage_is_reclaimed_with_the_next_attempt() {
        let retried = ev(
            4,
            Some("S1"),
            EventPayload::StageRetried { reason: "x".into() },
        );
        let plan = next_claim(&[submitted(), planned(), claimed(3, 1), retried]);
        assert_eq!(
            plan.map(|p| (p.existing_stage_id, p.attempt)),
            Some((Some("S1".to_owned()), 2))
        );
    }

    #[test]
    fn a_finished_job_has_nothing_to_claim() {
        let done = ev(5, None, EventPayload::JobFailed { reason: "x".into() });
        let failed = ev(
            4,
            Some("S1"),
            EventPayload::StageFailed { reason: "x".into() },
        );
        assert_eq!(
            next_claim(&[submitted(), planned(), claimed(3, 1), failed, done]),
            None
        );
    }

    #[test]
    fn a_job_parked_for_a_human_has_nothing_to_claim() {
        let parked = ev(2, None, EventPayload::JobNeedsHuman { reason: "x".into() });
        assert_eq!(next_claim(&[submitted(), parked]), None);
    }

    #[test]
    fn the_timeline_shows_each_stage_with_state_attempt_and_usage() {
        let usage = Usage {
            output_tokens: Some(8),
            ..Usage::default()
        };
        let events = [
            submitted(),
            planned(),
            claimed(3, 1),
            ev(
                4,
                Some("S1"),
                EventPayload::AgentFinished {
                    usage: usage.clone(),
                },
            ),
            ev(5, Some("S1"), EventPayload::StageSucceeded {}),
        ];
        assert_eq!(
            fold_stages(&events),
            vec![StageView {
                id: "S1".into(),
                n: 1,
                name: "implement".into(),
                state: StageState::Succeeded,
                attempt: 1,
                usage: Some(usage)
            }]
        );
    }
}
