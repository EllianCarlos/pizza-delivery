use serde::{Deserialize, Serialize};

use crate::context::SourceUse;
use crate::event::Event;
use crate::exit::ExitCode;
use crate::filter::{ArtifactFilter, ArtifactItem, EventFilter, InboxFilter, InboxItem};
use crate::id::JobId;
use crate::job::JobState;
use crate::quota::QuotaView;
use crate::schedule::Priority;
use crate::stage::StageView;
use crate::triage::Outcome;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    Submit {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        priority: Option<Priority>,
    },
    Cancel {
        job: String,
    },
    Triage {
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repo: Option<String>,
    },
    Status {
        job: Option<String>,
    },
    Quota,
    Inbox {
        #[serde(default)]
        filter: InboxFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Events {
        #[serde(default)]
        filter: EventFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Artifacts {
        #[serde(default)]
        filter: ArtifactFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobView {
    pub id: JobId,
    pub short_id: String,
    pub state: JobState,
    pub prompt: String,
    pub submitted_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    NotFound,
    Ambiguous,
    Usage,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub kind: ErrorKind,
    pub message: String,
    pub matches: Vec<JobId>,
}

impl ApiError {
    pub fn exit_code(&self) -> ExitCode {
        match self.kind {
            ErrorKind::NotFound | ErrorKind::Ambiguous => ExitCode::NotFound,
            ErrorKind::Usage => ExitCode::Usage,
            ErrorKind::Internal => ExitCode::General,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Response {
    Submitted {
        id: JobId,
        short_id: String,
    },
    Jobs {
        jobs: Vec<JobView>,
    },
    Job {
        job: JobView,
        stages: Vec<StageView>,
        events: Vec<Event>,
    },
    Quota {
        agents: Vec<QuotaView>,
        now_ms: i64,
    },
    Triaged {
        outcome: Outcome,
        context_sources: Vec<SourceUse>,
    },
    Cancelled {
        id: JobId,
        short_id: String,
    },
    InboxItems {
        items: Vec<InboxItem>,
    },
    EventItems {
        items: Vec<Event>,
    },
    ArtifactItems {
        items: Vec<ArtifactItem>,
    },
    Stopped,
    Error {
        error: ApiError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguity_and_not_found_both_exit_3() {
        let mk = |kind| ApiError {
            kind,
            message: String::new(),
            matches: vec![],
        };
        assert_eq!(mk(ErrorKind::Ambiguous).exit_code().code(), 3);
        assert_eq!(mk(ErrorKind::NotFound).exit_code().code(), 3);
        assert_eq!(mk(ErrorKind::Usage).exit_code().code(), 2);
        assert_eq!(mk(ErrorKind::Internal).exit_code().code(), 1);
    }

    #[test]
    fn requests_round_trip_as_tagged_json() {
        let req = Request::Status {
            job: Some("@last".into()),
        };
        let text = serde_json::to_string(&req).unwrap_or_default();
        assert_eq!(text, r#"{"op":"status","job":"@last"}"#);
        assert_eq!(serde_json::from_str::<Request>(&text).ok(), Some(req));
    }

    #[test]
    fn filter_requests_and_responses_round_trip_as_tagged_json() {
        let req = Request::Events {
            filter: EventFilter {
                kinds: vec!["stage-failed".into()],
                ..Default::default()
            },
            query: Some("boom".into()),
            limit: Some(5),
        };
        let text = serde_json::to_string(&req).unwrap_or_default();
        assert!(text.starts_with(r#"{"op":"events","filter":{"#), "{text}");
        assert_eq!(serde_json::from_str::<Request>(&text).ok(), Some(req));
        let bare: Result<Request, _> = serde_json::from_str(r#"{"op":"inbox"}"#);
        assert_eq!(
            bare.ok(),
            Some(Request::Inbox {
                filter: InboxFilter::default(),
                query: None,
                limit: None
            })
        );
        let resp = Response::ArtifactItems { items: vec![] };
        let text = serde_json::to_string(&resp).unwrap_or_default();
        assert_eq!(text, r#"{"type":"artifact-items","items":[]}"#);
        assert_eq!(serde_json::from_str::<Response>(&text).ok(), Some(resp));
    }
}
