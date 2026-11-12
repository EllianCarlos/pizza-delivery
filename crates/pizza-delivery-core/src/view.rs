use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::api::JobView;
use crate::budget::{SizeTier, StageEstimate};
use crate::event::{Actor, Event, EventPayload};
use crate::inbox::InboxEntry;
use crate::job::JobState;
use crate::quota::QuotaView;
use crate::stage::{fold_stages, StageView};
use crate::triage::current_plan;

pub const BOARD_COLUMNS: [JobState; 7] = [
    JobState::Queued,
    JobState::Running,
    JobState::WaitingForQuota,
    JobState::NeedsHuman,
    JobState::Completed,
    JobState::Failed,
    JobState::Cancelled,
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobsView {
    pub jobs: Vec<JobView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BoardColumn {
    pub state: JobState,
    pub jobs: Vec<JobView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BoardView {
    pub columns: Vec<BoardColumn>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanStageView {
    pub name: String,
    pub reason: String,
    pub tier: Option<SizeTier>,
    pub estimated_tokens: Option<u64>,
    pub estimated_wall_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PlanView {
    pub stages: Vec<PlanStageView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventView {
    pub cursor: u64,
    pub id: String,
    pub job_id: String,
    pub stage_id: Option<String>,
    pub seq: u64,
    pub ts_ms: i64,
    pub actor: Actor,
    pub kind: String,
    #[ts(type = "unknown")]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct EventsView {
    pub events: Vec<EventView>,
    pub next: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct JobDetailView {
    pub job: JobView,
    pub plan: PlanView,
    pub stages: Vec<StageView>,
    pub feedback: Vec<InboxEntry>,
    pub events: Vec<EventView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct QuotaPanelView {
    pub agents: Vec<QuotaView>,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct InboxView {
    pub entries: Vec<InboxEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SubmittedView {
    pub id: String,
    pub short_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RepliedView {
    pub id: String,
    pub short_id: String,
    pub answered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DismissedView {
    pub entries: Vec<String>,
}

pub fn jobs_view(jobs: &[JobView], state: Option<JobState>) -> JobsView {
    JobsView {
        jobs: jobs
            .iter()
            .filter(|j| state.map_or(true, |s| j.state == s))
            .cloned()
            .collect(),
    }
}

pub fn board_view(jobs: &[JobView]) -> BoardView {
    BoardView {
        columns: BOARD_COLUMNS
            .iter()
            .map(|&state| BoardColumn {
                state,
                jobs: jobs_view(jobs, Some(state)).jobs,
            })
            .collect(),
    }
}

pub fn event_view(cursor: u64, event: &Event) -> EventView {
    let payload = serde_json::to_value(event)
        .ok()
        .and_then(|mut v| v.get_mut("payload").map(serde_json::Value::take))
        .unwrap_or(serde_json::Value::Null);
    EventView {
        cursor,
        id: event.id.clone(),
        job_id: event.job_id.as_str().to_owned(),
        stage_id: event.stage_id.clone(),
        seq: event.seq,
        ts_ms: event.ts_ms,
        actor: event.actor,
        kind: event.payload.kind().to_owned(),
        payload,
    }
}

pub fn event_from_view(view: &EventView) -> Option<Event> {
    let payload = if view.payload.is_null() {
        serde_json::json!({})
    } else {
        view.payload.clone()
    };
    serde_json::from_value(serde_json::json!({
        "id": view.id,
        "job_id": view.job_id,
        "stage_id": view.stage_id,
        "seq": view.seq,
        "ts_ms": view.ts_ms,
        "actor": view.actor,
        "kind": view.kind,
        "payload": payload,
    }))
    .ok()
}

pub fn events_view(entries: &[(u64, Event)]) -> EventsView {
    EventsView {
        events: entries.iter().map(|(c, e)| event_view(*c, e)).collect(),
        next: entries.last().map_or(0, |(c, _)| *c),
    }
}

pub fn plan_view(events: &[Event]) -> PlanView {
    let estimates: Vec<&StageEstimate> = events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::BudgetEstimated { stages } => Some(stages),
            _ => None,
        })
        .next_back()
        .map(|stages| stages.iter().collect())
        .unwrap_or_default();
    PlanView {
        stages: current_plan(events)
            .into_iter()
            .map(|planned| {
                let estimate = estimates.iter().find(|e| e.stage == planned.name);
                PlanStageView {
                    name: planned.name,
                    reason: planned.reason,
                    tier: estimate.map(|e| e.tier),
                    estimated_tokens: estimate.map(|e| e.tokens),
                    estimated_wall_ms: estimate.map(|e| e.wall_ms),
                }
            })
            .collect(),
    }
}

pub fn job_detail(job: JobView, events: &[(u64, Event)], inbox: &[InboxEntry]) -> JobDetailView {
    let plain: Vec<Event> = events.iter().map(|(_, e)| e.clone()).collect();
    JobDetailView {
        feedback: inbox.iter().filter(|e| e.job == job.id).cloned().collect(),
        plan: plan_view(&plain),
        stages: fold_stages(&plain),
        events: events.iter().map(|(c, e)| event_view(*c, e)).collect(),
        job,
    }
}

pub fn quota_panel(agents: Vec<QuotaView>, now_ms: i64) -> QuotaPanelView {
    QuotaPanelView { agents, now_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::JobId;
    use crate::triage::{PlannedStage, Resolution, Template, Triage};

    fn job(id: &str, state: JobState) -> JobView {
        JobView {
            id: JobId::new(id),
            short_id: id.to_owned(),
            state,
            prompt: format!("prompt {id}"),
            submitted_at_ms: 1,
        }
    }

    fn event(seq: u64, payload: EventPayload) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: JobId::new("j1"),
            stage_id: None,
            seq,
            ts_ms: seq as i64,
            actor: Actor::Daemon,
            payload,
        }
    }

    fn triaged(stages: &[(&str, &str)]) -> EventPayload {
        EventPayload::TriageCompleted {
            triage: Triage {
                repo: Resolution {
                    path: "/p/r".into(),
                    confidence: 100,
                    alternatives: vec![],
                },
                template: Template::BugFix,
                plan: stages
                    .iter()
                    .map(|(name, reason)| PlannedStage {
                        name: (*name).into(),
                        reason: (*reason).into(),
                    })
                    .collect(),
                risk_flags: vec![],
                task: "t".into(),
            },
        }
    }

    fn estimated(stage: &str, tier: SizeTier, tokens: u64) -> EventPayload {
        EventPayload::BudgetEstimated {
            stages: vec![StageEstimate {
                stage: stage.into(),
                tier,
                tokens,
                wall_ms: tokens * 10,
            }],
        }
    }

    #[test]
    fn board_groups_jobs_into_one_column_per_state() {
        let jobs = [
            job("a", JobState::Queued),
            job("b", JobState::Running),
            job("c", JobState::Queued),
        ];
        let board = board_view(&jobs);
        let ids = |state: JobState| -> Vec<String> {
            board
                .columns
                .iter()
                .find(|c| c.state == state)
                .map(|c| c.jobs.iter().map(|j| j.short_id.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(board.columns.len(), 7);
        assert_eq!(ids(JobState::Queued), ["a", "c"]);
        assert_eq!(ids(JobState::Running), ["b"]);
        assert!(ids(JobState::Completed).is_empty());
    }

    #[test]
    fn jobs_filter_by_state() {
        let jobs = [job("a", JobState::Queued), job("b", JobState::Failed)];
        let only = jobs_view(&jobs, Some(JobState::Failed));
        assert_eq!(only.jobs.len(), 1);
        assert_eq!(only.jobs[0].short_id, "b");
        assert_eq!(jobs_view(&jobs, None).jobs.len(), 2);
    }

    #[test]
    fn plan_carries_reasons_and_the_latest_estimates() {
        let events = [
            event(1, triaged(&[("grill", "clarify"), ("implement", "build")])),
            event(2, estimated("grill", SizeTier::Small, 100)),
            event(3, estimated("grill", SizeTier::Medium, 900)),
        ];
        let plan = plan_view(&events);
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].reason, "clarify");
        assert_eq!(plan.stages[0].tier, Some(SizeTier::Medium));
        assert_eq!(plan.stages[0].estimated_tokens, Some(900));
        assert_eq!(plan.stages[0].estimated_wall_ms, Some(9000));
        assert_eq!(plan.stages[1].estimated_tokens, None);
    }

    #[test]
    fn plan_is_empty_before_triage() {
        assert!(plan_view(&[]).stages.is_empty());
    }

    #[test]
    fn event_view_exposes_cursor_kind_and_payload() {
        let e = event(
            3,
            EventPayload::JobFailed {
                reason: "boom".into(),
            },
        );
        let v = event_view(42, &e);
        assert_eq!(v.cursor, 42);
        assert_eq!(v.kind, "job-failed");
        assert_eq!(v.payload["reason"], "boom");
        assert_eq!(v.job_id, "j1");
    }

    #[test]
    fn an_event_view_converts_back_to_the_event_it_came_from() {
        let e = event(
            3,
            EventPayload::JobFailed {
                reason: "boom".into(),
            },
        );
        assert_eq!(event_from_view(&event_view(7, &e)), Some(e));
        let done = event(4, EventPayload::JobCompleted {});
        assert_eq!(event_from_view(&event_view(8, &done)), Some(done));
    }

    #[test]
    fn events_view_next_is_the_last_cursor() {
        let e = event(1, EventPayload::JobCompleted {});
        assert_eq!(events_view(&[(5, e.clone()), (9, e)]).next, 9);
        assert_eq!(events_view(&[]).next, 0);
    }

    #[test]
    fn job_detail_keeps_only_the_jobs_own_feedback() {
        let entry = |job: &str| InboxEntry {
            id: format!("f-{job}"),
            short_id: String::new(),
            job: JobId::new(job),
            kind: "question".into(),
            severity: "low".into(),
            text: "?".into(),
            confidence: None,
            alternatives: vec![],
            impact: None,
            evidence: None,
            status: crate::inbox::EntryStatus::Open,
            answered_by: None,
        };
        let detail = job_detail(
            job("j1", JobState::Queued),
            &[(1, event(1, triaged(&[("grill", "x")])))],
            &[entry("j1"), entry("j2")],
        );
        assert_eq!(detail.feedback.len(), 1);
        assert_eq!(detail.plan.stages.len(), 1);
        assert_eq!(detail.events[0].cursor, 1);
    }
}
