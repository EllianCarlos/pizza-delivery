use serde::{Deserialize, Serialize};

use crate::budget::{SizeTier, StageEstimate};
use crate::id::JobId;
use crate::quota::QuotaStatus;
use crate::resilience::RetryCause;
use crate::schedule::Priority;
use crate::triage::{PlannedStage, Triage};
use crate::usage::Usage;

fn is_normal(priority: &Priority) -> bool {
    *priority == Priority::Normal
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUp {
    pub parent: JobId,
    pub entries: Vec<String>,
    pub reply: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum Actor {
    Daemon,
    Agent,
    Human,
    RemoteHuman,
}

impl Actor {
    pub fn as_str(self) -> &'static str {
        match self {
            Actor::Daemon => "daemon",
            Actor::Agent => "agent",
            Actor::Human => "human",
            Actor::RemoteHuman => "remote-human",
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
        follow_up: Option<FollowUp>,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        alternatives: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        impact: Option<String>,
    },
    FeedbackDismissed {
        entry: String,
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
    McpRequested {
        server: String,
    },
    McpActivated {
        server: String,
        egress: Vec<String>,
        write: bool,
    },
    McpDenied {
        server: String,
        reason: String,
    },
    McpDeactivated {
        server: String,
        reason: String,
    },
    McpCalls {
        server: String,
        count: u64,
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
    WriteBack {
        store: String,
        id: String,
        topic: String,
        action: String,
    },
    RemoteAction {
        email: String,
        cf_ray: Option<String>,
        action: String,
    },
    AuthRejected {
        count: u32,
        reasons: std::collections::BTreeMap<String, u32>,
    },
}

pub const SYSTEM_JOB: &str = "system";

impl EventPayload {
    pub fn is_system(&self) -> bool {
        matches!(self, EventPayload::AuthRejected { .. })
    }

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
            EventPayload::FeedbackDismissed { .. } => "feedback-dismissed",
            EventPayload::PlanAmended { .. } => "plan-amended",
            EventPayload::JobNeedsHuman { .. } => "job-needs-human",
            EventPayload::BudgetEstimated { .. } => "budget-estimated",
            EventPayload::McpRequested { .. } => "mcp-requested",
            EventPayload::McpActivated { .. } => "mcp-activated",
            EventPayload::McpDenied { .. } => "mcp-denied",
            EventPayload::McpDeactivated { .. } => "mcp-deactivated",
            EventPayload::McpCalls { .. } => "mcp-calls",
            EventPayload::StageBudgetRecorded { .. } => "stage-budget-recorded",
            EventPayload::WriteBack { .. } => "write-back",
            EventPayload::RemoteAction { .. } => "remote-action",
            EventPayload::AuthRejected { .. } => "auth-rejected",
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
            follow_up: None,
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
}
