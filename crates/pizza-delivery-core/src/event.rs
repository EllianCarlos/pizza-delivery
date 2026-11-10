use serde::{Deserialize, Serialize};

use crate::id::JobId;
use crate::triage::{PlannedStage, Triage};
use crate::usage::Usage;

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
    },
    JobCompleted {},
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
}

impl EventPayload {
    pub fn kind(&self) -> &'static str {
        match self {
            EventPayload::JobSubmitted { .. } => "job-submitted",
            EventPayload::JobCompleted { .. } => "job-completed",
            EventPayload::JobFailed { .. } => "job-failed",
            EventPayload::StagePlanned { .. } => "stage-planned",
            EventPayload::StageClaimed { .. } => "stage-claimed",
            EventPayload::StageStarted { .. } => "stage-started",
            EventPayload::AgentInvoked { .. } => "agent-invoked",
            EventPayload::AgentFinished { .. } => "agent-finished",
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
        };
        assert_eq!(p.kind(), "job-submitted");
        assert_eq!(
            serde_json::to_value(&p).ok(),
            Some(serde_json::json!({"kind":"job-submitted","payload":{"prompt":"x"}}))
        );
    }

    #[test]
    fn payload_without_fields_round_trips_through_its_stored_form() {
        let stored = serde_json::json!({"kind":"stage-succeeded","payload":{}});
        let parsed: Result<EventPayload, _> = serde_json::from_value(stored);
        assert_eq!(parsed.ok(), Some(EventPayload::StageSucceeded {}));
    }
}
