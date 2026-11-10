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

    pub fn engram_type(self) -> &'static str {
        match self {
            Kind::Summary | Kind::Failure => "discovery",
            Kind::Convention => "pattern",
            Kind::Fix => "bugfix",
            Kind::Architecture => "architecture",
            Kind::Directive | Kind::CrossRepo => "manual",
        }
    }

    pub fn mnemon_category(self) -> &'static str {
        match self {
            Kind::Directive => "preference",
            Kind::CrossRepo => "fact",
            _ => "general",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Entry {
    pub kind: Kind,
    pub topic: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub id: String,
    pub topic: String,
    pub content: String,
    pub tags: Vec<String>,
}

impl Existing {
    pub fn is_ours(&self) -> bool {
        self.has_tag(OWN_TAG)
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }

    fn has_content(&self, content: &str) -> bool {
        same_content(&self.content, content)
    }
}

fn same_content(listed: &str, wanted: &str) -> bool {
    let (listed, wanted) = (listed.trim(), wanted.trim());
    if listed == wanted {
        return true;
    }
    match listed.strip_suffix("...") {
        Some(prefix) => wanted.starts_with(prefix.trim_end()),
        None => false,
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

pub fn job_tag(job: &str) -> String {
    format!("job:{job}")
}

pub fn own_marker(job: &str) -> String {
    format!("[{OWN_TAG} {}]", job_tag(job))
}

pub fn engram_topic_key(topic: &str) -> String {
    let slug: Vec<String> = topic
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    format!("{OWN_TAG}/{}", slug.join("-"))
}

pub fn engram_content(job: &str, entry: &Entry) -> String {
    format!("{} {}", own_marker(job), entry.content.trim())
}

pub fn mnemon_content(entry: &Entry) -> String {
    format!("{}\n{}", entry.topic.trim(), entry.content.trim())
}

pub fn mnemon_tags(job: &str) -> String {
    format!("{OWN_TAG},{}", job_tag(job))
}

fn split_marker(content: &str) -> (Vec<String>, String) {
    let unmarked = || (Vec::new(), content.to_owned());
    let Some((inner, rest)) = content
        .strip_prefix('[')
        .and_then(|after| after.split_once(']'))
    else {
        return unmarked();
    };
    let tags: Vec<String> = inner.split_whitespace().map(str::to_owned).collect();
    if tags.first().map(String::as_str) == Some(OWN_TAG) {
        (tags, rest.trim_start().to_owned())
    } else {
        unmarked()
    }
}

fn engram_header(line: &str) -> Option<(String, String)> {
    let (_, rest) = line.strip_prefix('[')?.split_once("] #")?;
    let (id, rest) = rest.split_once(' ')?;
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let (_, title) = rest.split_once(" \u{2014} ")?;
    Some((id.to_owned(), title.trim().to_owned()))
}

fn engram_meta(line: &str) -> bool {
    line.starts_with("    ") && line.contains(" | project: ")
}

pub fn parse_engram_search(text: &str) -> Vec<Existing> {
    let mut found = Vec::new();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let Some((id, topic)) = engram_header(line) else {
            continue;
        };
        let mut body: Vec<&str> = Vec::new();
        for next in lines.by_ref() {
            if engram_meta(next) {
                break;
            }
            body.push(next);
        }
        let joined = body.join("\n");
        let (tags, content) = split_marker(joined.strip_prefix("    ").unwrap_or(&joined));
        found.push(Existing {
            id,
            topic,
            content,
            tags,
        });
    }
    found
}

#[derive(Deserialize)]
struct MnemonInsight {
    id: String,
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Deserialize)]
struct MnemonHit {
    insight: MnemonInsight,
}

#[derive(Deserialize)]
struct MnemonRecall {
    #[serde(default)]
    results: Vec<MnemonHit>,
}

pub fn parse_mnemon_recall(text: &str) -> Vec<Existing> {
    serde_json::from_str::<MnemonRecall>(text)
        .map(|recall| {
            recall
                .results
                .into_iter()
                .map(|hit| {
                    let (topic, content) = hit
                        .insight
                        .content
                        .split_once('\n')
                        .unwrap_or((hit.insight.content.as_str(), ""));
                    Existing {
                        id: hit.insight.id.clone(),
                        topic: topic.trim().to_owned(),
                        content: content.trim().to_owned(),
                        tags: hit.insight.tags,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_engram_saved(text: &str) -> Option<String> {
    let after = text.split_once('#')?.1;
    let id: String = after.chars().take_while(char::is_ascii_digit).collect();
    (!id.is_empty()).then_some(id)
}

pub fn parse_mnemon_saved(text: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Saved {
        id: String,
    }
    serde_json::from_str::<Saved>(text).ok().map(|s| s.id)
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
        return if foreign.has_content(&entry.content) {
            Decision::Unchanged { id }
        } else {
            Decision::Contradiction { id }
        };
    }
    match matches.first() {
        Some(own) if own.has_content(&entry.content) => {
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

    const ENGRAM_SEARCH: &str = "Found 3 memories:\n\n[1] #2 (discovery) \u{2014} Second\n    [pizza-delivery job:J1] line one\nline two\n    2026-11-10 13:23:59 | project: my-repo | scope: project\n\n[2] #1 (architecture) \u{2014} Multi: line title\n    [pizza-delivery job:J1] word word word...\n    2026-11-10 13:23:59 | project: my-repo | scope: project\n\n[3] #7 (bugfix) \u{2014} Foreign \u{2014} fact\n    written by a person\n    2026-10-01 09:00:00 | project: my-repo | scope: project\n";

    #[test]
    fn real_engram_search_output_is_parsed_into_titles_content_and_job_tags() {
        let found = parse_engram_search(ENGRAM_SEARCH);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].id, "2");
        assert_eq!(found[0].topic, "Second");
        assert_eq!(found[0].content, "line one\nline two");
        assert_eq!(found[0].tags, vec!["pizza-delivery", "job:J1"]);
        assert!(found[0].is_ours() && found[0].has_tag("job:J1"));
        assert_eq!(found[2].topic, "Foreign \u{2014} fact");
        assert!(!found[2].is_ours());
        assert_eq!(found[2].content, "written by a person");
        assert!(parse_engram_search("No memories found for: \"x\"\n").is_empty());
    }

    #[test]
    fn a_truncated_engram_listing_still_matches_the_full_content() {
        let found = parse_engram_search(ENGRAM_SEARCH);
        let e = entry(Kind::Architecture, "Multi: line title", "word word word word");
        assert_eq!(
            decide(&e, &found, false),
            Decision::Unchanged { id: "1".into() }
        );
    }

    #[test]
    fn real_mnemon_recall_output_is_parsed_with_the_topic_from_the_first_line() {
        let out = r#"{"results":[{"insight":{"id":"abc","content":"Style\nuse tabs","category":"fact","importance":3,"tags":["pizza-delivery","job:J1"],"entities":[],"source":"agent"},"score":0.45,"intent":"GENERAL","via":"hybrid"}],"meta":{"hint":"sparse_results"}}"#;
        let found = parse_mnemon_recall(out);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "abc");
        assert_eq!(found[0].topic, "Style");
        assert_eq!(found[0].content, "use tabs");
        assert!(found[0].has_tag("job:J1") && found[0].is_ours());
        assert!(parse_mnemon_recall("[]").is_empty());
    }

    #[test]
    fn saved_ids_come_from_the_real_save_outputs() {
        assert_eq!(
            parse_engram_saved("Memory saved: #12 \"Login bug\" (bugfix)\n"),
            Some("12".to_owned())
        );
        assert_eq!(parse_engram_saved("boom"), None);
        assert_eq!(
            parse_mnemon_saved("{\n  \"action\": \"added\",\n  \"id\": \"x-1\"\n}"),
            Some("x-1".to_owned())
        );
    }

    #[test]
    fn the_engram_topic_key_is_a_stable_slug_and_the_marker_carries_the_job() {
        assert_eq!(
            engram_topic_key("  Login: bug / null-check! "),
            "pizza-delivery/login-bug-null-check"
        );
        assert_eq!(own_marker("J1"), "[pizza-delivery job:J1]");
        let e = entry(Kind::Fix, "t", " c ");
        assert_eq!(engram_content("J1", &e), "[pizza-delivery job:J1] c");
        assert_eq!(mnemon_content(&e), "t\nc");
        assert_eq!(mnemon_tags("J1"), "pizza-delivery,job:J1");
    }
}
