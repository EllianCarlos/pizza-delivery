//! Deterministic patterns mined from the ledger: where stages retry and fail,
//! what the owner does with feedback, how long stages take and what they cost,
//! and how often leases expire. No model is involved; the same events always
//! give the same report.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::store::{Store, StoreError};
use crate::suppress::is_no_issue_reply;

type CauseKey = (String, String, String, String);
const UNKNOWN: &str = "-";
const REASON_CHARS: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CausePattern {
    /// `stage-retried`, `stage-failed` or `job-failed`.
    pub event: String,
    pub stage: String,
    pub agent: String,
    pub cause: String,
    pub count: u32,
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobFeedback {
    pub job: String,
    pub recorded: u32,
    pub dismissed: u32,
    pub no_issue: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackRates {
    pub recorded: u32,
    pub dismissed: u32,
    pub no_issue: u32,
    pub dismissed_permille: u32,
    pub no_issue_permille: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageStat {
    pub stage: String,
    pub agent: String,
    pub runs: u32,
    pub total_ms: u64,
    pub avg_ms: u64,
    pub max_ms: u64,
    pub tokens: u64,
    pub cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseExpiries {
    /// The daemon that held the lease (`daemon-<pid>`): one per restart.
    pub holder: String,
    pub count: u32,
    pub jobs: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Insights {
    pub causes: Vec<CausePattern>,
    pub feedback_by_job: Vec<JobFeedback>,
    pub feedback: FeedbackRates,
    pub stages: Vec<StageStat>,
    pub lease_expiries: Vec<LeaseExpiries>,
}

/// Every event in the ledger, oldest first.
pub fn ledger_events(store: &dyn Store) -> Result<Vec<Event>, StoreError> {
    let mut out = Vec::new();
    let mut after = 0;
    loop {
        let page = store.events_since(after, 1000)?;
        let Some((last, _)) = page.last() else {
            return Ok(out);
        };
        after = *last;
        out.extend(page.into_iter().map(|(_, e)| e));
    }
}

fn reason_head(reason: &str) -> String {
    let first = reason.lines().next().unwrap_or("").trim();
    first.chars().take(REASON_CHARS).collect()
}

fn permille(part: u32, whole: u32) -> u32 {
    if whole == 0 {
        0
    } else {
        (u64::from(part) * 1000 / u64::from(whole)) as u32
    }
}

#[derive(Default)]
struct StageAcc {
    runs: u32,
    total_ms: u64,
    max_ms: u64,
    tokens: u64,
    cost: u64,
}

pub fn aggregate(events: &[Event]) -> Insights {
    let mut sorted: Vec<&Event> = events.iter().collect();
    sorted.sort_by_key(|e| e.seq);

    let names: HashMap<&str, &str> = sorted
        .iter()
        .filter_map(|e| match (&e.payload, e.stage_id.as_deref()) {
            (EventPayload::StagePlanned { name, .. }, Some(id)) => Some((id, name.as_str())),
            _ => None,
        })
        .collect();
    let stage_name = |e: &Event| {
        e.stage_id
            .as_deref()
            .and_then(|id| names.get(id).copied())
            .unwrap_or(UNKNOWN)
            .to_owned()
    };

    // stage id -> (agent, invoked at)
    let mut open: HashMap<String, (String, i64)> = HashMap::new();
    // stage id -> agent of the latest invocation, kept after the stage closes
    let mut last_agent: HashMap<String, String> = HashMap::new();
    let mut causes: BTreeMap<CauseKey, (u32, BTreeSet<String>)> = BTreeMap::new();
    let mut stats: BTreeMap<(String, String), StageAcc> = BTreeMap::new();
    let mut per_job: BTreeMap<String, JobFeedback> = BTreeMap::new();
    let mut leases: BTreeMap<String, (u32, BTreeSet<String>)> = BTreeMap::new();
    let mut recorded_by: HashMap<&str, &str> = HashMap::new();
    let mut counted_no_issue: BTreeSet<&str> = BTreeSet::new();

    for e in &sorted {
        let job = e.job_id.as_str();
        let stage = stage_name(e);
        let agent = e
            .stage_id
            .as_deref()
            .and_then(|id| last_agent.get(id))
            .cloned()
            .unwrap_or_else(|| UNKNOWN.to_owned());
        let mut bump = |event: &str, stage: String, agent: String, cause: String| {
            let slot = causes
                .entry((event.to_owned(), stage, agent, cause))
                .or_default();
            slot.0 += 1;
            slot.1.insert(job.to_owned());
        };
        let mut close = |stats: &mut BTreeMap<(String, String), StageAcc>| {
            if let Some((agent, started)) = e.stage_id.as_ref().and_then(|id| open.remove(id)) {
                let ms = (e.ts_ms - started).max(0) as u64;
                let acc = stats.entry((stage.clone(), agent)).or_default();
                acc.runs += 1;
                acc.total_ms += ms;
                acc.max_ms = acc.max_ms.max(ms);
            }
        };
        match &e.payload {
            EventPayload::AgentInvoked { agent, .. } => {
                if let Some(id) = &e.stage_id {
                    open.insert(id.clone(), (agent.clone(), e.ts_ms));
                    last_agent.insert(id.clone(), agent.clone());
                }
            }
            EventPayload::AgentFinished { usage } => {
                let agent = usage.agent.clone().unwrap_or(agent);
                let acc = stats.entry((stage, agent)).or_default();
                acc.tokens += usage.input_tokens.unwrap_or(0) + usage.output_tokens.unwrap_or(0);
                acc.cost += usage.cost_microusd.unwrap_or(0);
            }
            EventPayload::StageSucceeded {} => close(&mut stats),
            EventPayload::StageRetried { cause, .. } => {
                bump(
                    "stage-retried",
                    stage.clone(),
                    agent,
                    cause.as_str().to_owned(),
                );
                close(&mut stats);
            }
            EventPayload::StageFailed { reason } => {
                bump("stage-failed", stage.clone(), agent, reason_head(reason));
                close(&mut stats);
            }
            EventPayload::JobFailed { reason } => {
                bump(
                    "job-failed",
                    UNKNOWN.to_owned(),
                    UNKNOWN.to_owned(),
                    reason_head(reason),
                );
            }
            EventPayload::LeaseExpired { holder } => {
                let slot = leases.entry(holder.clone()).or_default();
                slot.0 += 1;
                slot.1.insert(job.to_owned());
            }
            EventPayload::FeedbackRecorded { .. } => {
                recorded_by.insert(e.id.as_str(), job);
                per_job.entry(job.to_owned()).or_default().recorded += 1;
            }
            EventPayload::FeedbackDismissed { entry } => {
                if let Some(owner) = recorded_by.get(entry.as_str()) {
                    per_job.entry((*owner).to_owned()).or_default().dismissed += 1;
                }
            }
            EventPayload::JobSubmitted {
                follow_up: Some(f), ..
            } if is_no_issue_reply(&f.reply) => {
                for entry in &f.entries {
                    if let Some(owner) = recorded_by.get(entry.as_str()) {
                        if counted_no_issue.insert(entry.as_str()) {
                            per_job.entry((*owner).to_owned()).or_default().no_issue += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut causes: Vec<CausePattern> = causes
        .into_iter()
        .map(
            |((event, stage, agent, cause), (count, jobs))| CausePattern {
                event,
                stage,
                agent,
                cause,
                count,
                jobs: jobs.into_iter().collect(),
            },
        )
        .collect();
    causes.sort_by_key(|a| std::cmp::Reverse(a.count));

    let feedback_by_job: Vec<JobFeedback> = per_job
        .into_iter()
        .map(|(job, f)| JobFeedback { job, ..f })
        .collect();
    let recorded = feedback_by_job.iter().map(|f| f.recorded).sum();
    let dismissed = feedback_by_job.iter().map(|f| f.dismissed).sum();
    let no_issue = feedback_by_job.iter().map(|f| f.no_issue).sum();

    let stages = stats
        .into_iter()
        .map(|((stage, agent), a)| StageStat {
            stage,
            agent,
            runs: a.runs,
            total_ms: a.total_ms,
            avg_ms: a.total_ms.checked_div(u64::from(a.runs)).unwrap_or(0),
            max_ms: a.max_ms,
            tokens: a.tokens,
            cost_microusd: a.cost,
        })
        .collect();

    let mut lease_expiries: Vec<LeaseExpiries> = leases
        .into_iter()
        .map(|(holder, (count, jobs))| LeaseExpiries {
            holder,
            count,
            jobs: jobs.into_iter().collect(),
        })
        .collect();
    lease_expiries.sort_by_key(|a| std::cmp::Reverse(a.count));

    Insights {
        causes,
        feedback_by_job,
        feedback: FeedbackRates {
            recorded,
            dismissed,
            no_issue,
            dismissed_permille: permille(dismissed, recorded),
            no_issue_permille: permille(no_issue, recorded),
        },
        stages,
        lease_expiries,
    }
}

/// A proposed improvement: a pattern that crossed the threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proposal {
    /// Stable across runs; embedded in the issue body to find duplicates.
    pub key: String,
    pub title: String,
    pub body: String,
}

pub const PROPOSAL_MARKER: &str = "pizza-delivery-insight:";

fn clip(jobs: &[String]) -> String {
    let shown: Vec<&str> = jobs.iter().take(10).map(String::as_str).collect();
    let more = jobs.len().saturating_sub(shown.len());
    let mut text = shown.join(", ");
    if more > 0 {
        text.push_str(&format!(" and {more} more"));
    }
    text
}

fn proposal(key: String, title: String, summary: String, jobs: &[String]) -> Proposal {
    let body = format!(
        "{summary}\n\nEvidence from the ledger (jobs): {}\n\nProposed by `pizza-delivery insights`. Triage before acting; nothing here is implemented automatically.\n\n<!-- {PROPOSAL_MARKER}{key} -->\n",
        clip(jobs)
    );
    Proposal { key, title, body }
}

/// Patterns at or above `threshold` occurrences, as issue-ready proposals.
pub fn proposals(insights: &Insights, threshold: u32) -> Vec<Proposal> {
    let mut out = Vec::new();
    for c in insights.causes.iter().filter(|c| c.count >= threshold) {
        let where_ = if c.stage == UNKNOWN {
            String::new()
        } else {
            format!(" in the {} stage (agent {})", c.stage, c.agent)
        };
        out.push(proposal(
            format!("{}:{}:{}:{}", c.event, c.stage, c.agent, c.cause),
            format!("Insight: {}{where_}: {}", c.event, c.cause),
            format!(
                "`{}` with cause `{}` happened {} times{where_}.",
                c.event, c.cause, c.count
            ),
            &c.jobs,
        ));
    }
    let expired: u32 = insights.lease_expiries.iter().map(|l| l.count).sum();
    if expired >= threshold {
        let jobs: Vec<String> = insights
            .lease_expiries
            .iter()
            .flat_map(|l| l.jobs.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        out.push(proposal(
            "lease-expired".to_owned(),
            "Insight: leases keep expiring".to_owned(),
            format!(
                "{expired} stage leases expired across {} daemon runs.",
                insights.lease_expiries.len()
            ),
            &jobs,
        ));
    }
    let f = &insights.feedback;
    let rejected = f.dismissed + f.no_issue;
    if rejected >= threshold && rejected * 2 >= f.recorded {
        let jobs: Vec<String> = insights
            .feedback_by_job
            .iter()
            .filter(|j| j.dismissed + j.no_issue > 0)
            .map(|j| j.job.clone())
            .collect();
        out.push(proposal(
            "feedback-noise".to_owned(),
            "Insight: most feedback is dismissed or answered 'no issue'".to_owned(),
            format!(
                "{rejected} of {} feedback entries were dismissed or answered 'no issue'. The agents may be raising noise.",
                f.recorded
            ),
            &jobs,
        ));
    }
    out
}

fn short(job: &str) -> &str {
    job.get(job.len().saturating_sub(8)..).unwrap_or(job)
}

fn secs(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// JSON lines (one object per row, tagged by `section`) or a readable table.
pub fn render_insights(insights: &Insights, json: bool) -> String {
    if json {
        let mut out = String::new();
        let mut push = |section: &str, value: serde_json::Value| {
            let mut value = value;
            if let Some(map) = value.as_object_mut() {
                map.insert("section".into(), section.into());
            }
            out.push_str(&serde_json::to_string(&value).unwrap_or_default());
            out.push('\n');
        };
        for c in &insights.causes {
            push("cause", serde_json::to_value(c).unwrap_or_default());
        }
        for j in &insights.feedback_by_job {
            push("feedback-job", serde_json::to_value(j).unwrap_or_default());
        }
        push(
            "feedback",
            serde_json::to_value(&insights.feedback).unwrap_or_default(),
        );
        for s in &insights.stages {
            push("stage", serde_json::to_value(s).unwrap_or_default());
        }
        for l in &insights.lease_expiries {
            push("lease", serde_json::to_value(l).unwrap_or_default());
        }
        return out;
    }
    let mut out = String::from("RETRIES AND FAILURES\n");
    if insights.causes.is_empty() {
        out.push_str("  none\n");
    }
    for c in &insights.causes {
        out.push_str(&format!(
            "  {}: {} x{}  ({}, agent {}, {} job(s))\n",
            c.stage,
            c.cause,
            c.count,
            c.event,
            c.agent,
            c.jobs.len()
        ));
    }
    let f = &insights.feedback;
    out.push_str(&format!(
        "\nFEEDBACK\n  {} recorded, {} dismissed ({}.{}%), {} answered 'no issue' ({}.{}%)\n",
        f.recorded,
        f.dismissed,
        f.dismissed_permille / 10,
        f.dismissed_permille % 10,
        f.no_issue,
        f.no_issue_permille / 10,
        f.no_issue_permille % 10,
    ));
    for j in &insights.feedback_by_job {
        out.push_str(&format!(
            "  {:<9} {} recorded, {} dismissed, {} no-issue\n",
            short(&j.job),
            j.recorded,
            j.dismissed,
            j.no_issue
        ));
    }
    out.push_str(&format!(
        "\nSTAGES\n  {:<12} {:<10} {:>5} {:>9} {:>9} {:>10} {:>9}\n",
        "STAGE", "AGENT", "RUNS", "AVG", "MAX", "TOKENS", "COST"
    ));
    for s in &insights.stages {
        out.push_str(&format!(
            "  {:<12} {:<10} {:>5} {:>9} {:>9} {:>10} {:>9}\n",
            s.stage,
            s.agent,
            s.runs,
            secs(s.avg_ms),
            secs(s.max_ms),
            s.tokens,
            format!("${:.4}", s.cost_microusd as f64 / 1_000_000.0)
        ));
    }
    out.push_str("\nLEASE EXPIRIES PER DAEMON RUN\n");
    if insights.lease_expiries.is_empty() {
        out.push_str("  none\n");
    }
    for l in &insights.lease_expiries {
        out.push_str(&format!("  {}: {}\n", l.holder, l.count));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, FollowUp};
    use crate::id::JobId;
    use crate::resilience::RetryCause;
    use crate::schedule::Priority;
    use crate::usage::Usage;

    struct Log {
        seq: u64,
        events: Vec<Event>,
    }

    impl Log {
        fn new() -> Self {
            Self {
                seq: 0,
                events: vec![],
            }
        }

        fn add(&mut self, job: &str, stage: Option<&str>, ts: i64, payload: EventPayload) {
            self.seq += 1;
            self.events.push(Event {
                id: format!("e{}", self.seq),
                job_id: JobId::new(job),
                stage_id: stage.map(str::to_owned),
                seq: self.seq,
                ts_ms: ts,
                actor: Actor::Daemon,
                payload,
            });
        }

        fn plan(&mut self, job: &str, stage: &str, name: &str) {
            self.add(
                job,
                Some(stage),
                0,
                EventPayload::StagePlanned {
                    n: 1,
                    name: name.into(),
                },
            );
        }

        fn invoke(&mut self, job: &str, stage: &str, ts: i64, agent: &str) {
            self.add(
                job,
                Some(stage),
                ts,
                EventPayload::AgentInvoked {
                    agent: agent.into(),
                    template: "t".into(),
                    version: 1,
                    prompt_sha256: "x".into(),
                },
            );
        }

        fn retry(&mut self, job: &str, stage: &str, ts: i64, cause: RetryCause) {
            self.add(
                job,
                Some(stage),
                ts,
                EventPayload::StageRetried {
                    reason: "r".into(),
                    cause,
                },
            );
        }
    }

    #[test]
    fn retries_group_by_stage_agent_and_cause_with_job_evidence() {
        let mut log = Log::new();
        for (job, stage) in [("j1", "s1"), ("j2", "s2"), ("j3", "s3")] {
            log.plan(job, stage, "review");
            log.invoke(job, stage, 10, "claude");
            log.retry(job, stage, 20, RetryCause::InvalidHandoff);
        }
        log.plan("j3", "s4", "spec");
        log.invoke("j3", "s4", 10, "claude");
        log.retry("j3", "s4", 20, RetryCause::Timeout);
        let report = aggregate(&log.events);
        assert_eq!(report.causes.len(), 2);
        let top = &report.causes[0];
        assert_eq!(
            (
                top.stage.as_str(),
                top.agent.as_str(),
                top.cause.as_str(),
                top.count
            ),
            ("review", "claude", "invalid-handoff", 3)
        );
        assert_eq!(top.jobs, vec!["j1", "j2", "j3"]);
    }

    #[test]
    fn failures_use_the_reason_head_and_job_failures_have_no_stage() {
        let mut log = Log::new();
        log.plan("j1", "s1", "implement");
        log.invoke("j1", "s1", 0, "codex");
        log.add(
            "j1",
            Some("s1"),
            5,
            EventPayload::StageFailed {
                reason: "boom\nsecond line".into(),
            },
        );
        log.add(
            "j1",
            None,
            6,
            EventPayload::JobFailed {
                reason: "out of budget".into(),
            },
        );
        let report = aggregate(&log.events);
        let kinds: Vec<(&str, &str, &str)> = report
            .causes
            .iter()
            .map(|c| (c.event.as_str(), c.stage.as_str(), c.cause.as_str()))
            .collect();
        assert!(kinds.contains(&("stage-failed", "implement", "boom")));
        assert!(kinds.contains(&("job-failed", "-", "out of budget")));
    }

    #[test]
    fn stage_durations_run_from_agent_invoked_to_the_next_stage_event_with_cost() {
        let mut log = Log::new();
        log.plan("j1", "s1", "spec");
        log.invoke("j1", "s1", 1_000, "claude");
        log.add(
            "j1",
            Some("s1"),
            1_500,
            EventPayload::AgentFinished {
                usage: Usage {
                    input_tokens: Some(100),
                    output_tokens: Some(50),
                    cost_microusd: Some(2_000),
                    agent: Some("claude".into()),
                    ..Default::default()
                },
            },
        );
        log.add("j1", Some("s1"), 4_000, EventPayload::StageSucceeded {});
        log.plan("j2", "s2", "spec");
        log.invoke("j2", "s2", 0, "claude");
        log.add("j2", Some("s2"), 1_000, EventPayload::StageSucceeded {});
        let report = aggregate(&log.events);
        assert_eq!(report.stages.len(), 1);
        let s = &report.stages[0];
        assert_eq!(
            (s.runs, s.total_ms, s.avg_ms, s.max_ms),
            (2, 4_000, 2_000, 3_000)
        );
        assert_eq!((s.tokens, s.cost_microusd), (150, 2_000));
    }

    #[test]
    fn feedback_rates_count_dismissals_and_no_issue_answers_once() {
        let mut log = Log::new();
        for id in ["a", "b", "c", "d"] {
            log.seq += 1;
            log.events.push(Event {
                id: id.into(),
                job_id: JobId::new("j1"),
                stage_id: None,
                seq: log.seq,
                ts_ms: 0,
                actor: Actor::Agent,
                payload: EventPayload::FeedbackRecorded {
                    kind: "risk".into(),
                    severity: "low".into(),
                    text: id.into(),
                    confidence: None,
                    alternatives: vec![],
                    impact: None,
                    evidence: None,
                },
            });
        }
        log.add(
            "j1",
            None,
            1,
            EventPayload::FeedbackDismissed { entry: "a".into() },
        );
        let answer = |reply: &str, entries: &[&str]| EventPayload::JobSubmitted {
            prompt: "p".into(),
            repo: None,
            priority: Priority::Normal,
            follow_up: Some(FollowUp {
                parent: JobId::new("j1"),
                entries: entries.iter().map(|s| (*s).to_owned()).collect(),
                reply: reply.into(),
            }),
            options: None,
        };
        log.add("j2", None, 2, answer("no issue", &["b"]));
        log.add("j3", None, 3, answer("no issue", &["b"]));
        log.add("j4", None, 4, answer("fix it", &["c"]));
        let report = aggregate(&log.events);
        assert_eq!(
            report.feedback,
            FeedbackRates {
                recorded: 4,
                dismissed: 1,
                no_issue: 1,
                dismissed_permille: 250,
                no_issue_permille: 250
            }
        );
        assert_eq!(report.feedback_by_job.len(), 1);
        assert_eq!(report.feedback_by_job[0].job, "j1");
    }

    #[test]
    fn lease_expiries_group_per_daemon_run() {
        let mut log = Log::new();
        for (job, holder) in [("j1", "daemon-1"), ("j2", "daemon-1"), ("j2", "daemon-2")] {
            log.add(
                job,
                Some("s"),
                0,
                EventPayload::LeaseExpired {
                    holder: holder.into(),
                },
            );
        }
        let report = aggregate(&log.events);
        assert_eq!(report.lease_expiries.len(), 2);
        assert_eq!(report.lease_expiries[0].holder, "daemon-1");
        assert_eq!(report.lease_expiries[0].count, 2);
    }

    #[test]
    fn proposals_need_the_threshold_and_carry_evidence_and_a_stable_key() {
        let mut log = Log::new();
        for (job, stage) in [("j1", "s1"), ("j2", "s2"), ("j3", "s3")] {
            log.plan(job, stage, "review");
            log.invoke(job, stage, 10, "claude");
            log.retry(job, stage, 20, RetryCause::InvalidHandoff);
        }
        let report = aggregate(&log.events);
        assert!(proposals(&report, 4).is_empty());
        let found = proposals(&report, 3);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "stage-retried:review:claude:invalid-handoff");
        assert!(found[0].title.contains("review"));
        assert!(found[0].body.contains("j1, j2, j3"));
        assert!(found[0]
            .body
            .contains(&format!("{PROPOSAL_MARKER}{}", found[0].key)));
    }

    #[test]
    fn the_same_events_in_any_order_give_the_same_report() {
        let mut log = Log::new();
        log.plan("j1", "s1", "review");
        log.invoke("j1", "s1", 10, "claude");
        log.retry("j1", "s1", 20, RetryCause::Crash);
        let mut shuffled = log.events.clone();
        shuffled.reverse();
        assert_eq!(aggregate(&log.events), aggregate(&shuffled));
    }

    #[test]
    fn rendering_gives_json_lines_and_a_table() {
        let mut log = Log::new();
        log.plan("j1", "s1", "review");
        log.invoke("j1", "s1", 0, "claude");
        log.retry("j1", "s1", 10, RetryCause::InvalidHandoff);
        let report = aggregate(&log.events);
        let json = render_insights(&report, true);
        assert!(json.lines().all(|l| l.starts_with('{')), "{json}");
        assert!(json.contains(r#""section":"cause""#));
        let table = render_insights(&report, false);
        assert!(table.contains("review: invalid-handoff x1"), "{table}");
    }
}
