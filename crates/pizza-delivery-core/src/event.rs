use serde::{Deserialize, Serialize};

use crate::budget::{SizeTier, StageEstimate};
use crate::id::JobId;
use crate::quota::QuotaStatus;
use crate::resilience::RetryCause;
use crate::schedule::Priority;
use crate::triage::{PlannedStage, Triage, TriageOptions};
use crate::usage::Usage;

fn is_normal(priority: &Priority) -> bool {
    *priority == Priority::Normal
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Actor {
    Daemon,
    Agent,
    Human,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Actor::Daemon => "daemon",
            Actor::Agent => "agent",
            Actor::Human => "human",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "kebab-case")]
pub enum EventPayload {
    JobSubmitted {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, skip_serializing_if = "is_normal")]
        priority: Priority,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<TriageOptions>,
    },
    JobCompleted {},
    JobCancelled {},
    JobFailed {
        reason: String,
    },
    StagePlanned {
        n: u32,
        name: String,
    },
    StageClaimed {
        holder: String,
        attempt: u32,
        lease_expires_ms: i64,
    },
    StageStarted {
        attempt: u32,
    },
    AgentInvoked {
        agent: String,
        template: String,
        version: u32,
        prompt_sha256: String,
    },
    AgentFinished {
        usage: Usage,
    },
    QuotaSignal {
        agent: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        status: QuotaStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        utilization_permille: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resets_at_ms: Option<i64>,
        reset_known: bool,
    },
    AgentFellBack {
        agent: String,
        reason: String,
    },
    JobParked {
        until_ms: i64,
        reason: String,
    },
    SandboxAudit {
        profile_hash: String,
        mounts: Vec<String>,
        egress_allowlist: Vec<String>,
        blocked_requests: Vec<String>,
        mcp_activations: Vec<String>,
        diff_policy_result: Option<String>,
        pushed_ref: Option<String>,
        pr_url: Option<String>,
    },
    EgressBlocked {
        host: String,
    },
    ArtifactRecorded {
        kind: String,
        path: String,
        sha256: String,
    },
    HandoffWritten {
        path: String,
    },
    StageSucceeded {},
    StageFailed {
        reason: String,
    },
    StageRetried {
        reason: String,
        #[serde(default)]
        cause: RetryCause,
    },
    LeaseExpired {
        holder: String,
    },
    LeaseReleased {
        holder: String,
    },
    ContextRead {
        source: String,
        bytes: u64,
    },
    TriageCompleted {
        triage: Triage,
    },
    FeedbackRecorded {
        kind: String,
        severity: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        confidence: Option<u8>,
    },
    PlanAmended {
        after: String,
        stages: Vec<PlannedStage>,
    },
    JobNeedsHuman {
        reason: String,
    },
    BudgetEstimated {
        stages: Vec<StageEstimate>,
    },
    StageBudgetRecorded {
        stage: String,
        tier: SizeTier,
        agent: String,
        estimate_tokens: u64,
        estimate_wall_ms: u64,
        actual_tokens: Option<u64>,
        actual_wall_ms: Option<u64>,
    },
}

impl EventPayload {
    pub fn kind(&self) -> &'static str {
        match self {
            EventPayload::JobSubmitted { .. } => "job-submitted",
            EventPayload::JobCompleted { .. } => "job-completed",
            EventPayload::JobFailed { .. } => "job-failed",
            EventPayload::JobCancelled { .. } => "job-cancelled",
            EventPayload::StagePlanned { .. } => "stage-planned",
            EventPayload::StageClaimed { .. } => "stage-claimed",
            EventPayload::StageStarted { .. } => "stage-started",
            EventPayload::AgentInvoked { .. } => "agent-invoked",
            EventPayload::AgentFinished { .. } => "agent-finished",
            EventPayload::QuotaSignal { .. } => "quota-signal",
            EventPayload::AgentFellBack { .. } => "agent-fell-back",
            EventPayload::JobParked { .. } => "job-parked",
            EventPayload::SandboxAudit { .. } => "sandbox-audit",
            EventPayload::EgressBlocked { .. } => "egress-blocked",
            EventPayload::ArtifactRecorded { .. } => "artifact-recorded",
            EventPayload::HandoffWritten { .. } => "handoff-written",
            EventPayload::StageSucceeded { .. } => "stage-succeeded",
            EventPayload::StageFailed { .. } => "stage-failed",
            EventPayload::StageRetried { .. } => "stage-retried",
            EventPayload::LeaseExpired { .. } => "lease-expired",
            EventPayload::LeaseReleased { .. } => "lease-released",
            EventPayload::ContextRead { .. } => "context-read",
            EventPayload::TriageCompleted { .. } => "triage-completed",
            EventPayload::FeedbackRecorded { .. } => "feedback-recorded",
            EventPayload::PlanAmended { .. } => "plan-amended",
            EventPayload::JobNeedsHuman { .. } => "job-needs-human",
            EventPayload::BudgetEstimated { .. } => "budget-estimated",
            EventPayload::StageBudgetRecorded { .. } => "stage-budget-recorded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub job_id: JobId,
    pub stage_id: Option<String>,
    pub seq: u64,
    pub ts_ms: i64,
    pub actor: Actor,
    #[serde(flatten)]
    pub payload: EventPayload,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_serializes_with_kind_tag() {
        let p = EventPayload::JobSubmitted {
            prompt: "x".into(),
            repo: None,
            priority: Priority::Normal,
            options: None,
        };
        assert_eq!(p.kind(), "job-submitted");
        assert_eq!(
            serde_json::to_value(&p).ok(),
            Some(serde_json::json!({"kind":"job-submitted","payload":{"prompt":"x"}}))
        );
    }

    #[test]
    fn audit_fields_are_always_present_even_when_unknown() {
        let p = EventPayload::SandboxAudit {
            profile_hash: "h".into(),
            mounts: vec![],
            egress_allowlist: vec![],
            blocked_requests: vec![],
            mcp_activations: vec![],
            diff_policy_result: None,
            pushed_ref: None,
            pr_url: None,
        };
        let v = serde_json::to_value(&p).unwrap_or_default();
        for key in [
            "profile_hash",
            "mounts",
            "egress_allowlist",
            "blocked_requests",
            "mcp_activations",
            "diff_policy_result",
            "pushed_ref",
            "pr_url",
        ] {
            assert!(v["payload"].get(key).is_some(), "missing {key}");
        }
    }

    #[test]
    fn payload_without_fields_round_trips_through_its_stored_form() {
        let stored = serde_json::json!({"kind":"stage-succeeded","payload":{}});
        let parsed: Result<EventPayload, _> = serde_json::from_value(stored);
        assert_eq!(parsed.ok(), Some(EventPayload::StageSucceeded {}));
    }

    #[test]
    fn job_submitted_without_options_replays_as_none_and_options_round_trip(
    ) -> Result<(), serde_json::Error> {
        let old: EventPayload =
            serde_json::from_str(r#"{"kind":"job-submitted","payload":{"prompt":"x"}}"#)?;
        assert!(matches!(
            old,
            EventPayload::JobSubmitted { options: None, .. }
        ));
        let with = EventPayload::JobSubmitted {
            prompt: "x".into(),
            repo: None,
            priority: Priority::Normal,
            options: Some(crate::triage::TriageOptions {
                context_hint: Some("hint".into()),
                ..Default::default()
            }),
        };
        let back: EventPayload = serde_json::from_str(&serde_json::to_string(&with)?)?;
        assert_eq!(back, with);
        Ok(())
    }
}
