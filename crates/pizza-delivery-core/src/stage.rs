use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::triage::current_plan;
use crate::usage::Usage;

pub const STAGE_NAME: &str = "implement";

pub const PIPELINE_STAGES: [&str; 6] =
    ["grill", "spec", "investigate", "implement", "fix", "review"];

pub fn pipeline(events: &[Event]) -> Vec<String> {
    let triaged = events
        .iter()
        .any(|e| matches!(e.payload, EventPayload::TriageCompleted { .. }));
    let runnable: Vec<String> = current_plan(events)
        .into_iter()
        .map(|s| s.name)
        .filter(|name| PIPELINE_STAGES.contains(&name.as_str()))
        .collect();
    if triaged && !runnable.is_empty() {
        runnable
    } else {
        vec![STAGE_NAME.to_owned()]
    }
}

pub fn fix_rounds(events: &[Event]) -> u32 {
    let rounds = fold_stages(events)
        .iter()
        .filter(|s| s.name == "fix")
        .count();
    u32::try_from(rounds).unwrap_or(u32::MAX)
}

pub fn pipeline_done(events: &[Event]) -> bool {
    let stages = fold_stages(events);
    stages.len() >= pipeline(events).len()
        && stages
            .last()
            .is_some_and(|s| s.state == StageState::Succeeded)
}

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
                EventPayload::JobParked { .. } => {
                    stage.state = StageState::Queued;
                    stage.attempt = stage.attempt.saturating_sub(1);
                }
                EventPayload::StageSucceeded {} => stage.state = StageState::Succeeded,
                EventPayload::StageFailed { .. } => stage.state = StageState::Failed,
                EventPayload::AgentFinished { usage } => stage.usage = Some(usage.clone()),
                _ => {}
            }
        }
        stages
    })
}

fn parked_until(events: &[Event]) -> Option<i64> {
    events
        .iter()
        .rev()
        .take_while(|e| !matches!(e.payload, EventPayload::StageClaimed { .. }))
        .find_map(|e| match &e.payload {
            EventPayload::JobParked { until_ms, .. } => Some(*until_ms),
            _ => None,
        })
}

pub fn next_claim(events: &[Event], now_ms: i64) -> Option<ClaimPlan> {
    if parked_until(events).is_some_and(|until| until > now_ms) {
        return None;
    }
    let finished = events.iter().any(|e| {
        matches!(
            e.payload,
            EventPayload::JobCompleted {}
                | EventPayload::JobFailed { .. }
                | EventPayload::JobNeedsHuman { .. }
                | EventPayload::JobCancelled {}
        )
    });
    if finished {
        return None;
    }
    let stages = fold_stages(events);
    let planned = pipeline(events);
    match stages.last().cloned() {
        None => planned.first().map(|name| ClaimPlan {
            existing_stage_id: None,
            n: 1,
            name: name.clone(),
            attempt: 1,
        }),
        Some(stage) if stage.state == StageState::Succeeded => {
            planned.get(stages.len()).map(|name| ClaimPlan {
                existing_stage_id: None,
                n: stage.n + 1,
                name: name.clone(),
                attempt: 1,
            })
        }
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
                priority: Default::default(),
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
            next_claim(&[submitted()], 0),
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
        assert_eq!(
            next_claim(&[submitted(), planned(), claimed(3, 1)], 0),
            None
        );
    }

    #[test]
    fn a_retried_stage_is_reclaimed_with_the_next_attempt() {
        let retried = ev(
            4,
            Some("S1"),
            EventPayload::StageRetried {
                reason: "x".into(),
                cause: Default::default(),
            },
        );
        let plan = next_claim(&[submitted(), planned(), claimed(3, 1), retried], 0);
        assert_eq!(
            plan.map(|p| (p.existing_stage_id, p.attempt)),
            Some((Some("S1".to_owned()), 2))
        );
    }

    fn parked(seq: u64, until_ms: i64) -> Event {
        ev(
            seq,
            Some("S1"),
            EventPayload::JobParked {
                until_ms,
                reason: "quota".into(),
            },
        )
    }

    #[test]
    fn a_parked_stage_is_claimable_again_only_at_its_reset_and_keeps_its_attempt_number() {
        let events = [submitted(), planned(), claimed(3, 1), parked(4, 1_000)];
        assert_eq!(next_claim(&events, 999), None);
        assert_eq!(
            next_claim(&events, 1_000).map(|p| (p.existing_stage_id, p.attempt)),
            Some((Some("S1".to_owned()), 1))
        );
    }

    #[test]
    fn a_claim_after_a_park_ends_the_park() {
        let events = [
            submitted(),
            planned(),
            claimed(3, 1),
            parked(4, 1_000),
            claimed(5, 1),
        ];
        assert_eq!(next_claim(&events, 0), None);
        assert_eq!(
            fold_stages(&events).last().map(|s| s.state),
            Some(StageState::Running)
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
            next_claim(&[submitted(), planned(), claimed(3, 1), failed, done], 0),
            None
        );
    }

    #[test]
    fn a_job_parked_for_a_human_has_nothing_to_claim() {
        let parked = ev(2, None, EventPayload::JobNeedsHuman { reason: "x".into() });
        assert_eq!(next_claim(&[submitted(), parked], 0), None);
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

    fn triaged(names: &[&str]) -> Event {
        use crate::triage::{PlannedStage, Resolution, Template, Triage};
        ev(
            2,
            None,
            EventPayload::TriageCompleted {
                triage: Triage {
                    repo: Resolution {
                        path: "/r".into(),
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

    fn planned_n(seq: u64, id: &str, n: u32, name: &str) -> Event {
        ev(
            seq,
            Some(id),
            EventPayload::StagePlanned {
                n,
                name: name.into(),
            },
        )
    }

    fn succeeded(seq: u64, id: &str) -> Event {
        ev(seq, Some(id), EventPayload::StageSucceeded {})
    }

    #[test]
    fn a_triaged_job_runs_its_plan_in_order_skipping_stages_without_a_template() {
        let mut events = vec![
            submitted(),
            triaged(&["grill", "implement", "review", "ship"]),
        ];
        let first = next_claim(&events, 0);
        assert_eq!(
            first.map(|p| (p.n, p.name, p.existing_stage_id)),
            Some((1, "grill".to_owned(), None))
        );
        events.extend([planned_n(3, "S1", 1, "grill"), succeeded(4, "S1")]);
        assert_eq!(
            next_claim(&events, 0).map(|p| (p.n, p.name, p.attempt)),
            Some((2, "implement".to_owned(), 1))
        );
        events.extend([planned_n(5, "S2", 2, "implement"), succeeded(6, "S2")]);
        events.extend([planned_n(7, "S3", 3, "review"), succeeded(8, "S3")]);
        assert_eq!(next_claim(&events, 0), None);
        assert!(pipeline_done(&events));
    }

    #[test]
    fn a_job_without_triage_runs_the_single_implement_stage() {
        assert_eq!(pipeline(&[submitted()]), vec!["implement".to_owned()]);
        assert!(!pipeline_done(&[submitted()]));
        assert!(pipeline_done(&[submitted(), planned(), succeeded(3, "S1")]));
    }

    #[test]
    fn an_amended_plan_adds_the_fix_and_re_review_stages_and_counts_fix_rounds() {
        use crate::triage::PlannedStage;
        let stage = |n: &str| PlannedStage {
            name: n.into(),
            reason: "r".into(),
        };
        let mut events = vec![
            submitted(),
            triaged(&["implement", "review", "ship"]),
            planned_n(3, "S1", 1, "implement"),
            succeeded(4, "S1"),
            planned_n(5, "S2", 2, "review"),
            succeeded(6, "S2"),
        ];
        assert!(pipeline_done(&events));
        events.push(ev(
            7,
            None,
            EventPayload::PlanAmended {
                after: "review".into(),
                stages: vec![stage("fix"), stage("review")],
            },
        ));
        assert!(!pipeline_done(&events));
        assert_eq!(fix_rounds(&events), 0);
        assert_eq!(
            next_claim(&events, 0).map(|p| p.name),
            Some("fix".to_owned())
        );
        events.extend([planned_n(8, "S3", 3, "fix"), succeeded(9, "S3")]);
        assert_eq!(fix_rounds(&events), 1);
        assert_eq!(
            next_claim(&events, 0).map(|p| (p.n, p.name)),
            Some((4, "review".to_owned()))
        );
    }
}
