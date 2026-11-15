//! Per-repo suppression rules learned from the owner's own answers.
//!
//! A feedback entry the owner dismissed, or answered with a "no issue" reply,
//! becomes a rule: the same kind of entry with the same text in the same repo
//! is not recorded again. Rules are derived from the ledger, so they are
//! visible, and the owner removes one with a `suppression-removed` event.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::event::{Event, EventPayload};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub repo: String,
    pub kind: String,
    pub text: String,
    /// `dismissed` or `no-issue`.
    pub reason: String,
    pub entry: String,
    pub job: String,
}

/// Collapse case, whitespace and surrounding punctuation so entries that differ
/// only by spacing match the same rule.
pub fn normalize(text: &str) -> String {
    let joined = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    joined
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned()
}

/// Whether a reply tells the agent the entry is not a problem.
pub fn is_no_issue_reply(text: &str) -> bool {
    let n = normalize(text);
    ["no issue", "not an issue", "no problem", "not a problem"]
        .iter()
        .any(|p| {
            n == *p
                || n.strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with([' ', ',', '.', '!']))
        })
}

pub fn rule_id(repo: &str, kind: &str, text: &str) -> String {
    let digest = Sha256::digest(format!("{repo}\n{kind}\n{}", normalize(text)).as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// The repo each job works in: the triaged repo when there is one, else the
/// submitted one.
pub fn job_repos(events: &[Event]) -> HashMap<String, String> {
    let mut repos: HashMap<String, String> = HashMap::new();
    for e in events {
        let job = e.job_id.as_str().to_owned();
        match &e.payload {
            EventPayload::JobSubmitted {
                repo: Some(repo), ..
            } => {
                repos.entry(job).or_insert_with(|| repo.clone());
            }
            EventPayload::TriageCompleted { triage } => {
                repos.insert(job, triage.repo.path.clone());
            }
            _ => {}
        }
    }
    repos
}

/// Active rules, in the order they were learned. Events may arrive in any order.
pub fn rules(events: &[Event]) -> Vec<Rule> {
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.seq);
    let repos = job_repos(events);
    let mut entries: HashMap<&str, (&str, &str, &str)> = HashMap::new();
    let mut active: Vec<Rule> = Vec::new();
    for e in sorted {
        let mut learned: Vec<(&str, &str)> = Vec::new();
        match &e.payload {
            EventPayload::FeedbackRecorded { kind, text, .. } => {
                entries.insert(e.id.as_str(), (e.job_id.as_str(), kind, text));
            }
            EventPayload::FeedbackDismissed { entry } => learned.push((entry, "dismissed")),
            EventPayload::JobSubmitted {
                follow_up: Some(f), ..
            } if is_no_issue_reply(&f.reply) => {
                learned.extend(f.entries.iter().map(|id| (id.as_str(), "no-issue")));
            }
            EventPayload::SuppressionRemoved { rule } => active.retain(|r| &r.id != rule),
            _ => {}
        }
        for (entry_id, reason) in learned {
            let Some((job, kind, text)) = entries.get(entry_id) else {
                continue;
            };
            let Some(repo) = repos.get(*job) else {
                continue;
            };
            let id = rule_id(repo, kind, text);
            if active.iter().any(|r| r.id == id) {
                continue;
            }
            active.push(Rule {
                id,
                repo: repo.clone(),
                kind: (*kind).to_owned(),
                text: (*text).to_owned(),
                reason: reason.to_owned(),
                entry: entry_id.to_owned(),
                job: (*job).to_owned(),
            });
        }
    }
    active
}

pub fn matching<'a>(
    rules: &'a [Rule],
    repo: Option<&str>,
    kind: &str,
    text: &str,
) -> Option<&'a Rule> {
    let id = rule_id(repo?, kind, text);
    rules.iter().find(|r| r.id == id)
}

/// Find one rule by id or unique id prefix.
pub fn resolve_rule<'a>(rules: &'a [Rule], reference: &str) -> Result<&'a Rule, Vec<&'a Rule>> {
    let hits: Vec<&Rule> = rules
        .iter()
        .filter(|r| !reference.is_empty() && r.id.starts_with(reference))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one),
        _ => Err(hits),
    }
}

pub fn render_rules(rules: &[Rule], json: bool) -> String {
    if json {
        return rules
            .iter()
            .map(|r| format!("{}\n", serde_json::to_string(r).unwrap_or_default()))
            .collect();
    }
    if rules.is_empty() {
        return "no suppression rules\n".to_owned();
    }
    let rows: String = rules
        .iter()
        .map(|r| {
            format!(
                "{:<8}  {:<9}  {:<10}  {}  {}\n",
                r.id, r.reason, r.kind, r.repo, r.text
            )
        })
        .collect();
    format!(
        "{:<8}  {:<9}  {:<10}  REPO  TEXT\n{rows}",
        "ID", "REASON", "KIND"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, FollowUp};
    use crate::id::JobId;
    use crate::schedule::Priority;

    fn ev(seq: u64, id: &str, job: &str, payload: EventPayload) -> Event {
        Event {
            id: id.into(),
            job_id: JobId::new(job),
            stage_id: None,
            seq,
            ts_ms: seq as i64,
            actor: Actor::Human,
            payload,
        }
    }

    fn submitted(seq: u64, job: &str, repo: &str, follow_up: Option<FollowUp>) -> Event {
        ev(
            seq,
            &format!("s-{job}"),
            job,
            EventPayload::JobSubmitted {
                prompt: "p".into(),
                repo: Some(repo.into()),
                priority: Priority::Normal,
                follow_up,
                options: None,
            },
        )
    }

    fn fb(seq: u64, id: &str, job: &str, text: &str) -> Event {
        ev(
            seq,
            id,
            job,
            EventPayload::FeedbackRecorded {
                kind: "risk".into(),
                severity: "low".into(),
                text: text.into(),
                confidence: None,
                alternatives: vec![],
                impact: None,
                evidence: None,
            },
        )
    }

    fn dismiss(seq: u64, id: &str, entry: &str) -> Event {
        ev(
            seq,
            id,
            "j1",
            EventPayload::FeedbackDismissed {
                entry: entry.into(),
            },
        )
    }

    #[test]
    fn no_issue_replies_are_recognised() {
        assert!(is_no_issue_reply("No issue."));
        assert!(is_no_issue_reply("not an issue, carry on"));
        assert!(!is_no_issue_reply("there is no issue tracker here"));
        assert!(!is_no_issue_reply("please fix"));
    }

    #[test]
    fn a_dismissed_entry_becomes_a_rule_for_its_repo_only() {
        let events = vec![
            submitted(1, "j1", "/r", None),
            fb(2, "e1", "j1", "Uses  unwrap in tests"),
            dismiss(3, "d1", "e1"),
        ];
        let rules = rules(&events);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].reason, "dismissed");
        assert!(matching(&rules, Some("/r"), "risk", "uses unwrap in tests.").is_some());
        assert!(matching(&rules, Some("/other"), "risk", "uses unwrap in tests").is_none());
        assert!(matching(&rules, Some("/r"), "assumption", "uses unwrap in tests").is_none());
        assert!(matching(&rules, None, "risk", "uses unwrap in tests").is_none());
    }

    #[test]
    fn a_no_issue_answer_becomes_a_rule_and_other_answers_do_not() {
        let answered = |reply: &str| FollowUp {
            parent: JobId::new("j1"),
            entries: vec!["e1".into()],
            reply: reply.into(),
        };
        let base = vec![submitted(1, "j1", "/r", None), fb(2, "e1", "j1", "x")];
        let mut yes = base.clone();
        yes.push(submitted(3, "j2", "/r", Some(answered("no issue"))));
        assert_eq!(rules(&yes)[0].reason, "no-issue");
        let mut no = base;
        no.push(submitted(3, "j2", "/r", Some(answered("please fix it"))));
        assert!(rules(&no).is_empty());
    }

    #[test]
    fn removal_drops_the_rule_and_a_later_dismissal_restores_it() {
        let mut events = vec![
            submitted(1, "j1", "/r", None),
            fb(2, "e1", "j1", "x"),
            dismiss(3, "d1", "e1"),
        ];
        let id = rules(&events)[0].id.clone();
        events.push(ev(
            4,
            "r1",
            "j1",
            EventPayload::SuppressionRemoved { rule: id.clone() },
        ));
        assert!(rules(&events).is_empty());
        events.push(fb(5, "e2", "j1", "x"));
        events.push(dismiss(6, "d2", "e2"));
        let again = rules(&events);
        assert_eq!(again.len(), 1);
        assert_eq!(
            resolve_rule(&again, &id[..4]).map(|r| r.id.clone()).ok(),
            Some(id)
        );
    }
}
