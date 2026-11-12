use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::id::JobId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum EntryStatus {
    Open,
    Answered,
    Dismissed,
}

impl EntryStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            EntryStatus::Open => "open",
            EntryStatus::Answered => "answered",
            EntryStatus::Dismissed => "dismissed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct InboxEntry {
    pub id: String,
    pub short_id: String,
    pub job: JobId,
    pub kind: String,
    pub severity: String,
    pub text: String,
    pub confidence: Option<u8>,
    pub alternatives: Vec<String>,
    pub impact: Option<String>,
    pub status: EntryStatus,
    pub answered_by: Option<JobId>,
}

pub fn fold_inbox<'a>(events: impl IntoIterator<Item = &'a Event>) -> Vec<InboxEntry> {
    let mut entries: Vec<InboxEntry> = Vec::new();
    let mut dismissed: Vec<&str> = Vec::new();
    let mut answered: HashMap<&str, &JobId> = HashMap::new();
    for event in events {
        match &event.payload {
            EventPayload::FeedbackRecorded {
                kind,
                severity,
                text,
                confidence,
                alternatives,
                impact,
                ..
            } => entries.push(InboxEntry {
                id: event.id.clone(),
                short_id: String::new(),
                job: event.job_id.clone(),
                kind: kind.clone(),
                severity: severity.clone(),
                text: text.clone(),
                confidence: *confidence,
                alternatives: alternatives.clone(),
                impact: impact.clone(),
                status: EntryStatus::Open,
                answered_by: None,
            }),
            EventPayload::FeedbackDismissed { entry } => dismissed.push(entry),
            EventPayload::JobSubmitted {
                follow_up: Some(follow_up),
                ..
            } => {
                for entry in &follow_up.entries {
                    answered.insert(entry, &event.job_id);
                }
            }
            _ => {}
        }
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    let ids: Vec<JobId> = entries.iter().map(|e| JobId::new(e.id.clone())).collect();
    for entry in &mut entries {
        entry.short_id = crate::id::short_id(&JobId::new(entry.id.clone()), &ids);
        if let Some(by) = answered.get(entry.id.as_str()) {
            entry.status = EntryStatus::Answered;
            entry.answered_by = Some((*by).clone());
        } else if dismissed.contains(&entry.id.as_str()) {
            entry.status = EntryStatus::Dismissed;
        }
    }
    entries
}

pub fn follow_up_prompt(parent_prompt: &str, answered: &[&InboxEntry], reply: &str) -> String {
    let entries: String = answered
        .iter()
        .map(|e| format!("- [{}] {}\n", e.kind, e.text))
        .collect();
    format!(
        "Follow-up to an earlier job.\n\nOriginal prompt:\n{parent_prompt}\n\nFeedback entries answered:\n{entries}\nReply:\n{reply}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, FollowUp};
    use crate::schedule::Priority;

    fn event(id: &str, job: &str, payload: EventPayload) -> Event {
        Event {
            id: id.into(),
            job_id: JobId::new(job),
            stage_id: None,
            seq: 1,
            ts_ms: 0,
            actor: Actor::Agent,
            payload,
        }
    }

    fn feedback(text: &str) -> EventPayload {
        EventPayload::FeedbackRecorded {
            kind: "assumption".into(),
            severity: "low".into(),
            text: text.into(),
            confidence: None,
            alternatives: vec![],
            impact: None,
            evidence: None,
        }
    }

    #[test]
    fn entries_are_open_until_answered_by_a_follow_up_or_dismissed() {
        let history = [
            event("E1", "J1", feedback("a")),
            event("E2", "J1", feedback("b")),
            event("E3", "J1", feedback("c")),
            event(
                "J2",
                "J2",
                EventPayload::JobSubmitted {
                    prompt: "p".into(),
                    repo: None,
                    priority: Priority::High,
                    follow_up: Some(FollowUp {
                        parent: JobId::new("J1"),
                        entries: vec!["E1".into()],
                        reply: "r".into(),
                    }),
                    options: None,
                },
            ),
            event(
                "D1",
                "J1",
                EventPayload::FeedbackDismissed { entry: "E2".into() },
            ),
        ];

        let states: Vec<(EntryStatus, Option<JobId>)> = fold_inbox(&history)
            .into_iter()
            .map(|e| (e.status, e.answered_by))
            .collect();

        assert_eq!(
            states,
            vec![
                (EntryStatus::Answered, Some(JobId::new("J2"))),
                (EntryStatus::Dismissed, None),
                (EntryStatus::Open, None),
            ]
        );
    }
}
