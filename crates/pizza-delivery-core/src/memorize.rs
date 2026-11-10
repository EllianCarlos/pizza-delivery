use serde::Deserialize;

use crate::event::{Event, EventPayload};
use crate::resilience::is_cancelled;
use crate::triage::Feedback;

pub const OWN_TAG: &str = "pizza-delivery";
pub const ARTIFACT_KIND: &str = "memory-writeback";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Summary,
    Convention,
    Fix,
    Failure,
    Architecture,
    Directive,
    CrossRepo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Store {
    Engram,
    Mnemon,
}

impl Store {
    pub fn as_str(self) -> &'static str {
        match self {
            Store::Engram => "engram",
            Store::Mnemon => "mnemon",
        }
    }
}

impl Kind {
    pub fn store(self) -> Store {
        match self {
            Kind::Directive | Kind::CrossRepo => Store::Mnemon,
            _ => Store::Engram,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub topic: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Existing {
    pub id: String,
    #[serde(default)]
    pub topic: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Existing {
    pub fn is_ours(&self) -> bool {
        self.has_tag(OWN_TAG)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

fn parse_lines<T: for<'de> Deserialize<'de>>(text: &str) -> Vec<T> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l.trim()).ok())
        .collect()
}

pub fn parse_plan(text: &str) -> Vec<Entry> {
    parse_lines::<Entry>(text)
        .into_iter()
        .filter(|e| !e.topic.trim().is_empty() && !e.content.trim().is_empty())
        .collect()
}

pub fn parse_existing(text: &str) -> Vec<Existing> {
    parse_lines(text)
}

pub fn job_tag(job: &str) -> String {
    format!("job:{job}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Save,
    Update { id: String },
    Unchanged { id: String },
    Contradiction { id: String },
    Unconfirmed,
}

pub fn decide(entry: &Entry, existing: &[Existing], confirmed_by_reply: bool) -> Decision {
    if entry.kind == Kind::Directive && !confirmed_by_reply {
        return Decision::Unconfirmed;
    }
    let matches: Vec<&Existing> = existing
        .iter()
        .filter(|e| e.topic.trim().eq_ignore_ascii_case(entry.topic.trim()))
        .collect();
    if let Some(foreign) = matches.iter().find(|e| !e.is_ours()) {
        let id = foreign.id.clone();
        return if foreign.content.trim() == entry.content.trim() {
            Decision::Unchanged { id }
        } else {
            Decision::Contradiction { id }
        };
    }
    match matches.first() {
        Some(own) if own.content.trim() == entry.content.trim() => {
            Decision::Unchanged { id: own.id.clone() }
        }
        Some(own) => Decision::Update { id: own.id.clone() },
        None => Decision::Save,
    }
}

pub fn contradiction_risk(entry: &Entry, store: Store, id: &str) -> Feedback {
    Feedback {
        kind: "risk".to_owned(),
        severity: "medium".to_owned(),
        text: format!(
            "Memory '{}' in {} (id {id}) was not written by pizza-delivery and differs from what this job learned: {}",
            entry.topic,
            store.as_str(),
            entry.content.trim()
        ),
        confidence: None,
        alternatives: vec![],
        impact: Some(
            "the existing memory was left untouched and the new fact was not saved".to_owned(),
        ),
    }
}

pub fn should_memorize(events: &[Event]) -> bool {
    let ended = events.iter().any(|e| {
        matches!(
            e.payload,
            EventPayload::JobCompleted {}
                | EventPayload::JobFailed { .. }
                | EventPayload::JobNeedsHuman { .. }
        )
    });
    let done = events.iter().any(|e| {
        matches!(&e.payload, EventPayload::ArtifactRecorded { kind, .. } if kind == ARTIFACT_KIND)
    });
    ended && !is_cancelled(events) && !done
}

pub fn prompt(
    job: &str,
    task: &str,
    outcome: &str,
    handoffs: &[(String, String)],
    plan_path: &str,
) -> String {
    let mut out = format!(
        "You are running unattended. Never ask the human anything.\n\nJob {job} has ended ({outcome}). Decide what is worth remembering for future jobs. Save little but durable facts: one summary of the job, repository conventions, bug fixes, architecture facts, and what failed and why. Never record guesses or assumed owner preferences. Record a directive only when the owner stated it in a reply.\n\nDo not run any memory command yourself. Write one JSON object per line to {plan_path} with the fields kind (summary, convention, fix, failure, architecture, directive or cross-repo), topic (a short stable title, reused when the fact is updated) and content.\n\nTask:\n{task}\n"
    );
    for (stage, text) in handoffs {
        out.push_str(&format!("\nHandoff from {stage}:\n{text}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::id::JobId;

    fn entry(kind: Kind, topic: &str, content: &str) -> Entry {
        Entry {
            kind,
            topic: topic.to_owned(),
            content: content.to_owned(),
        }
    }

    fn existing(id: &str, topic: &str, content: &str, ours: bool) -> Existing {
        Existing {
            id: id.to_owned(),
            topic: topic.to_owned(),
            content: content.to_owned(),
            tags: if ours {
                vec![OWN_TAG.to_owned()]
            } else {
                vec![]
            },
        }
    }

    fn event(seq: u64, payload: EventPayload) -> Event {
        Event {
            id: format!("E{seq}"),
            job_id: JobId::new("J1"),
            stage_id: None,
            seq,
            ts_ms: 0,
            actor: Actor::Daemon,
            payload,
        }
    }

    #[test]
    fn project_facts_go_to_engram_and_directives_and_cross_repo_facts_to_mnemon() {
        for kind in [
            Kind::Summary,
            Kind::Convention,
            Kind::Fix,
            Kind::Failure,
            Kind::Architecture,
        ] {
            assert_eq!(kind.store(), Store::Engram);
        }
        assert_eq!(Kind::Directive.store(), Store::Mnemon);
        assert_eq!(Kind::CrossRepo.store(), Store::Mnemon);
    }

    #[test]
    fn the_plan_keeps_valid_lines_and_drops_the_rest() {
        let plan = parse_plan(
            "{\"kind\":\"fix\",\"topic\":\"t\",\"content\":\"c\"}\nnot json\n{\"kind\":\"bogus\",\"topic\":\"t\",\"content\":\"c\"}\n{\"kind\":\"fix\",\"topic\":\" \",\"content\":\"c\"}\n",
        );
        assert_eq!(plan, vec![entry(Kind::Fix, "t", "c")]);
    }

    #[test]
    fn an_unknown_topic_is_saved() {
        let e = entry(Kind::Fix, "t", "c");
        let others = [existing("1", "other", "c", true)];
        assert_eq!(decide(&e, &others, false), Decision::Save);
    }

    #[test]
    fn our_own_memory_on_the_same_topic_is_updated_not_duplicated() {
        let e = entry(Kind::Fix, "T", "new");
        assert_eq!(
            decide(&e, &[existing("1", "t", "old", true)], false),
            Decision::Update { id: "1".into() }
        );
        assert_eq!(
            decide(&e, &[existing("1", "t", "new", true)], false),
            Decision::Unchanged { id: "1".into() }
        );
    }

    #[test]
    fn a_differing_foreign_memory_is_a_contradiction_and_is_never_overwritten() {
        let e = entry(Kind::Convention, "t", "new");
        let both = [
            existing("9", "t", "old", false),
            existing("1", "t", "old", true),
        ];
        assert_eq!(
            decide(&e, &both, false),
            Decision::Contradiction { id: "9".into() }
        );
        assert_eq!(
            decide(&e, &[existing("9", "t", "new", false)], false),
            Decision::Unchanged { id: "9".into() }
        );
    }

    #[test]
    fn a_directive_needs_a_reply_to_confirm_it() {
        let e = entry(Kind::Directive, "t", "c");
        assert_eq!(decide(&e, &[], false), Decision::Unconfirmed);
        assert_eq!(decide(&e, &[], true), Decision::Save);
    }

    #[test]
    fn the_contradiction_risk_names_the_foreign_memory() {
        let risk = contradiction_risk(&entry(Kind::Fix, "t", "new"), Store::Engram, "9");
        assert_eq!(risk.kind, "risk");
        assert!(risk.text.contains("engram"));
        assert!(risk.text.contains('9'));
        assert!(risk.text.contains("new"));
    }

    #[test]
    fn memorize_runs_after_completed_failed_and_needs_human_jobs_but_not_cancelled_ones() {
        let ended = |p: EventPayload| vec![event(1, p)];
        let reason = || "r".to_owned();
        assert!(should_memorize(&ended(EventPayload::JobCompleted {})));
        assert!(should_memorize(&ended(EventPayload::JobFailed {
            reason: reason()
        })));
        assert!(should_memorize(&ended(EventPayload::JobNeedsHuman {
            reason: reason()
        })));
        assert!(!should_memorize(&ended(EventPayload::JobCancelled {})));
        assert!(!should_memorize(&[]));
        let cancelled_after_fail = vec![
            event(1, EventPayload::JobFailed { reason: reason() }),
            event(2, EventPayload::JobCancelled {}),
        ];
        assert!(!should_memorize(&cancelled_after_fail));
    }

    #[test]
    fn memorize_runs_only_once() {
        let events = vec![
            event(1, EventPayload::JobCompleted {}),
            event(
                2,
                EventPayload::ArtifactRecorded {
                    kind: ARTIFACT_KIND.into(),
                    path: "p".into(),
                    sha256: "s".into(),
                },
            ),
        ];
        assert!(!should_memorize(&events));
    }
}
