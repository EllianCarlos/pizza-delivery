use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
}

pub const LOW_CONFIDENCE: u8 = 70;
pub const TIE_MARGIN: u8 = 10;
pub const MIN_SCORE: u8 = 50;
pub const TOP_CANDIDATES: usize = 3;
const MIN_TOKEN: usize = 4;
const MIN_SIMILARITY: usize = 75;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub name: String,
    pub path: String,
    pub score: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub path: String,
    pub confidence: u8,
    pub alternatives: Vec<Candidate>,
}

impl Resolution {
    pub fn is_low(&self) -> bool {
        self.confidence < LOW_CONFIDENCE
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cascade {
    Decided(Resolution),
    Tie(Vec<Candidate>),
    NoCandidate,
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let width = b.len() + 1;
    let mut d = vec![0usize; (a.len() + 1) * width];
    for i in 0..=a.len() {
        d[i * width] = i;
    }
    for (j, cell) in d.iter_mut().enumerate().take(b.len() + 1) {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[(i - 1) * width + j] + 1)
                .min(d[i * width + j - 1] + 1)
                .min(d[(i - 1) * width + j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[(i - 2) * width + j - 2] + 1);
            }
            d[i * width + j] = best;
        }
    }
    d[a.len() * width + b.len()]
}

fn similarity(a: &str, b: &str) -> usize {
    let longest = a.chars().count().max(b.chars().count());
    if longest == 0 {
        return 0;
    }
    100 - 100 * edit_distance(a, b) / longest
}

fn score(prompt_tokens: &[String], name: &str) -> u8 {
    let parts = tokens(name);
    if parts.is_empty() {
        return 0;
    }
    if prompt_tokens
        .windows(parts.len())
        .any(|window| window == parts.as_slice())
    {
        return 100;
    }
    let joined = parts.concat();
    let whole = prompt_tokens
        .iter()
        .filter(|t| t.chars().count() >= MIN_TOKEN)
        .map(|t| similarity(t, &joined))
        .max()
        .unwrap_or(0);
    let weight = if parts.len() == 1 { 100 } else { 80 };
    let partial = parts
        .iter()
        .filter(|p| p.chars().count() >= MIN_TOKEN)
        .flat_map(|p| {
            prompt_tokens
                .iter()
                .filter(|t| t.chars().count() >= MIN_TOKEN)
                .map(|t| similarity(t, p))
        })
        .max()
        .unwrap_or(0)
        * weight
        / 100;
    let best = [whole, partial]
        .into_iter()
        .filter(|s| *s >= MIN_SIMILARITY * weight / 100)
        .max()
        .unwrap_or(0);
    u8::try_from(best).unwrap_or(100)
}

fn explicit(path: &str) -> Resolution {
    Resolution {
        path: path.to_owned(),
        confidence: 100,
        alternatives: vec![],
    }
}

fn candidates(prompt: &str, index: &[Project]) -> Vec<Candidate> {
    let prompt_tokens = tokens(prompt);
    let mut found: Vec<Candidate> = index
        .iter()
        .map(|p| Candidate {
            name: p.name.clone(),
            path: p.path.clone(),
            score: score(&prompt_tokens, &p.name),
        })
        .filter(|c| c.score >= MIN_SCORE)
        .collect();
    found.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
    found
}

fn resolution_of(chosen: &Candidate, rest: impl Iterator<Item = Candidate>) -> Resolution {
    Resolution {
        path: chosen.path.clone(),
        confidence: chosen.score,
        alternatives: rest.collect(),
    }
}

pub fn rank(flag: Option<&str>, prompt: &str, index: &[Project]) -> Cascade {
    if let Some(path) = flag {
        return Cascade::Decided(explicit(path));
    }
    let found = candidates(prompt, index);
    match found.as_slice() {
        [] => Cascade::NoCandidate,
        [only] => Cascade::Decided(resolution_of(only, std::iter::empty())),
        [first, second, ..] if first.score.saturating_sub(second.score) <= TIE_MARGIN => {
            Cascade::Tie(found.into_iter().take(TOP_CANDIDATES).collect())
        }
        [first, rest @ ..] => Cascade::Decided(resolution_of(first, rest.iter().cloned())),
    }
}

pub fn break_tie(top: &[Candidate], choice: Option<usize>) -> Resolution {
    let valid = choice.filter(|i| *i < top.len());
    let index = valid.unwrap_or(0);
    let rest = top
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != index)
        .map(|(_, c)| c.clone());
    match (top.get(index), valid) {
        (Some(chosen), Some(_)) => resolution_of(chosen, rest),
        (Some(chosen), None) => Resolution {
            confidence: chosen.score.min(LOW_CONFIDENCE.saturating_sub(1)),
            ..resolution_of(chosen, rest)
        },
        (None, _) => Resolution {
            path: String::new(),
            confidence: 0,
            alternatives: vec![],
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Template {
    FullFeature,
    BugFix,
    Investigate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedStage {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Triage {
    pub repo: Resolution,
    pub template: Template,
    pub plan: Vec<PlannedStage>,
    pub risk_flags: Vec<String>,
    pub task: String,
}

pub const DEFAULT_MAX_STAGES: usize = 10;

const INVESTIGATE_WORDS: [&str; 7] = [
    "investigate",
    "research",
    "explore",
    "look into",
    "figure out",
    "find out",
    "should we",
];
const BUG_WORDS: [&str; 8] = [
    "fix",
    "bug",
    "broken",
    "crash",
    "regression",
    "flaky",
    "fails",
    "failing",
];
const RISK_WORDS: [(&str, &[&str]); 4] = [
    (
        "destructive-change",
        &[
            "drop ",
            "delete ",
            "wipe",
            "purge",
            "force push",
            "reset --hard",
        ],
    ),
    ("production", &["production", "prod "]),
    ("secrets", &["secret", "credential", "password", "token"]),
    ("data-migration", &["migration", "migrate"]),
];

fn has_any(haystack: &str, words: &[&str]) -> bool {
    words.iter().any(|w| haystack.contains(w))
}

pub fn classify(prompt: &str) -> Template {
    let lower = prompt.to_lowercase();
    if has_any(&lower, &INVESTIGATE_WORDS) {
        Template::Investigate
    } else if has_any(&lower, &BUG_WORDS) {
        Template::BugFix
    } else {
        Template::FullFeature
    }
}

fn stage(name: &str, reason: &str) -> PlannedStage {
    PlannedStage {
        name: name.to_owned(),
        reason: reason.to_owned(),
    }
}

pub fn plan_for(template: Template) -> Vec<PlannedStage> {
    let grill = stage(
        "grill",
        "settle open questions from context before building",
    );
    let implement = stage("implement", "make the change in the job worktree");
    let review = stage(
        "review",
        "check the change against the task and conventions",
    );
    let ship = stage("ship", "push the branch and open the draft pull request");
    match template {
        Template::FullFeature => vec![
            grill,
            stage(
                "spec",
                "write the spec and task tickets the implementation follows",
            ),
            implement,
            review,
            ship,
        ],
        Template::BugFix => vec![grill, implement, review, ship],
        Template::Investigate => vec![
            stage("investigate", "gather findings before deciding anything"),
            stage("grill", "decide whether a pull request is needed"),
            stage("implement", "make the change if the decision calls for one"),
            review,
            ship,
        ],
    }
}

pub fn risk_flags(prompt: &str) -> Vec<String> {
    let lower = format!("{} ", prompt.to_lowercase());
    RISK_WORDS
        .iter()
        .filter(|(_, words)| has_any(&lower, words))
        .map(|(flag, _)| (*flag).to_owned())
        .collect()
}

fn restate(prompt: &str, repo: &Resolution, template: Template) -> String {
    let flat = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "Task: {flat}\nRepository: {}\nApproach: {}",
        repo.path,
        match template {
            Template::FullFeature => "build a new capability from spec to pull request",
            Template::BugFix => "diagnose and fix a defect, then ship the fix",
            Template::Investigate => "investigate first, then decide whether a change is needed",
        }
    )
}

pub fn triage(prompt: &str, repo: Resolution) -> Triage {
    let template = classify(prompt);
    Triage {
        task: restate(prompt, &repo, template),
        plan: plan_for(template),
        risk_flags: risk_flags(prompt),
        template,
        repo,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AmendError {
    ExceedsBound { max: usize, would_be: usize },
    UnknownStage(String),
}

pub fn amend_plan(
    current: &[PlannedStage],
    after: &str,
    added: Vec<PlannedStage>,
    max: usize,
) -> Result<Vec<PlannedStage>, AmendError> {
    let position = current
        .iter()
        .position(|s| s.name == after)
        .ok_or_else(|| AmendError::UnknownStage(after.to_owned()))?;
    let would_be = current.len() + added.len();
    if would_be > max {
        return Err(AmendError::ExceedsBound { max, would_be });
    }
    let mut plan = current.to_vec();
    plan.splice(position + 1..position + 1, added);
    Ok(plan)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Feedback {
    pub kind: String,
    pub severity: String,
    pub text: String,
    pub confidence: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub impact: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub triage: Option<Triage>,
    pub feedback: Vec<Feedback>,
}

pub fn resolve_repo(
    flag: Option<&str>,
    prompt: &str,
    index: &[Project],
    tie_break: &dyn Fn(&[Candidate]) -> Option<usize>,
) -> Option<Resolution> {
    match rank(flag, prompt, index) {
        Cascade::Decided(resolution) => Some(resolution),
        Cascade::Tie(top) => Some(break_tie(&top, tie_break(&top))),
        Cascade::NoCandidate => None,
    }
}

pub fn conclude(prompt: &str, resolution: Option<Resolution>) -> Outcome {
    match resolution {
        None => Outcome {
            triage: None,
            feedback: vec![Feedback {
                kind: "needs-human".to_owned(),
                severity: "high".to_owned(),
                text: "no repository matched the prompt; pass --repo or name the project"
                    .to_owned(),
                confidence: None,
                alternatives: Vec::new(),
                impact: None,
            }],
        },
        Some(resolution) => {
            let feedback = if resolution.is_low() {
                vec![Feedback {
                    kind: "assumption".to_owned(),
                    severity: "medium".to_owned(),
                    text: format!(
                        "assumed the repository is {} (best of {} candidates)",
                        resolution.path,
                        resolution.alternatives.len() + 1
                    ),
                    confidence: Some(resolution.confidence),
                    alternatives: Vec::new(),
                    impact: None,
                }]
            } else {
                vec![]
            };
            Outcome {
                triage: Some(triage(prompt, resolution)),
                feedback,
            }
        }
    }
}

pub fn tiebreak_prompt(prompt: &str, top: &[Candidate]) -> String {
    let options: String = top
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{}. {} ({})\n", i + 1, c.name, c.path))
        .collect();
    format!(
        "Which repository does this task belong to? Reply with only the number of the best option.\n\nTask: {prompt}\n\nOptions:\n{options}"
    )
}

pub fn reply_text(stdout: &str) -> String {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v.get("type").and_then(|t| t.as_str()) == Some("result"))
        .filter_map(|v| v.get("result").and_then(|r| r.as_str()).map(str::to_owned))
        .next_back()
        .unwrap_or_default()
}

pub fn parse_choice(reply: &str, options: usize) -> Option<usize> {
    reply
        .split(|c: char| !c.is_ascii_digit())
        .find(|t| !t.is_empty())
        .and_then(|t| t.parse::<usize>().ok())
        .filter(|n| (1..=options).contains(n))
        .map(|n| n - 1)
}

pub fn render_triage(outcome: &Outcome, json: bool) -> String {
    if json {
        return format!("{}\n", serde_json::to_string(outcome).unwrap_or_default());
    }
    let mut out = String::new();
    match &outcome.triage {
        Some(t) => {
            out.push_str(&format!(
                "repo: {} (confidence {}%)\n",
                t.repo.path, t.repo.confidence
            ));
            for alt in &t.repo.alternatives {
                out.push_str(&format!("  alternative: {} ({}%)\n", alt.path, alt.score));
            }
            out.push_str("plan:\n");
            for (i, s) in t.plan.iter().enumerate() {
                out.push_str(&format!("  {}. {}: {}\n", i + 1, s.name, s.reason));
            }
            if !t.risk_flags.is_empty() {
                out.push_str(&format!("risks: {}\n", t.risk_flags.join(", ")));
            }
            out.push_str(&format!("{}\n", t.task));
        }
        None => out.push_str("no repository resolved\n"),
    }
    for f in &outcome.feedback {
        out.push_str(&format!("{} ({}): {}\n", f.kind, f.severity, f.text));
    }
    out
}

pub fn current_plan(events: &[Event]) -> Vec<PlannedStage> {
    events
        .iter()
        .fold(Vec::new(), |plan, event| match &event.payload {
            EventPayload::TriageCompleted { triage } => triage.plan.clone(),
            EventPayload::PlanAmended { after, stages } => {
                amend_plan(&plan, after, stages.clone(), DEFAULT_MAX_STAGES).unwrap_or(plan)
            }
            _ => plan,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project {
            name: name.to_owned(),
            path: format!("/p/{name}"),
        }
    }

    fn index() -> Vec<Project> {
        vec![
            project("pizza-delivery"),
            project("blog"),
            project("pizza-web"),
        ]
    }

    fn decided(cascade: Cascade) -> Option<Resolution> {
        match cascade {
            Cascade::Decided(r) => Some(r),
            _ => None,
        }
    }

    fn tied(cascade: Cascade) -> Vec<Candidate> {
        match cascade {
            Cascade::Tie(top) => top,
            _ => vec![],
        }
    }

    #[test]
    fn an_explicit_repo_wins_with_full_confidence_and_no_alternatives() {
        let cascade = rank(Some("/elsewhere/repo"), "fix pizza-delivery", &index());
        assert_eq!(
            cascade,
            Cascade::Decided(Resolution {
                path: "/elsewhere/repo".into(),
                confidence: 100,
                alternatives: vec![]
            })
        );
    }

    #[test]
    fn a_project_named_in_the_prompt_is_resolved_with_alternatives() {
        let found = decided(rank(None, "add retries to pizza-delivery daemon", &index()));
        assert_eq!(
            found.as_ref().map(|r| r.path.as_str()),
            Some("/p/pizza-delivery")
        );
        assert_eq!(found.as_ref().map(|r| r.confidence), Some(100));
        let names: Vec<String> = found
            .map(|r| r.alternatives.into_iter().map(|c| c.name).collect())
            .unwrap_or_default();
        assert_eq!(names, vec!["pizza-web".to_owned()]);
    }

    #[test]
    fn a_misspelled_name_still_resolves_by_fuzzy_match() {
        let found = decided(rank(None, "tidy the blgo styles", &index()));
        assert_eq!(found.as_ref().map(|r| r.path.as_str()), Some("/p/blog"));
        assert_eq!(found.map(|r| r.confidence < 100), Some(true));
    }

    #[test]
    fn near_equal_candidates_go_to_the_model_tie_break() {
        let top = tied(rank(None, "fix the pizza ordering", &index()));
        let names: Vec<&str> = top.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["pizza-delivery", "pizza-web"]);
    }

    #[test]
    fn a_prompt_matching_nothing_has_no_candidate() {
        assert_eq!(
            rank(None, "do something unrelated", &index()),
            Cascade::NoCandidate
        );
        assert_eq!(rank(None, "anything", &[]), Cascade::NoCandidate);
    }

    #[test]
    fn a_model_choice_resolves_a_tie_and_no_choice_falls_back_to_low_confidence() {
        let top = tied(rank(None, "fix the pizza ordering", &index()));
        let chosen = break_tie(&top, Some(1));
        assert_eq!(chosen.path, "/p/pizza-web");
        assert!(!chosen.is_low());
        assert_eq!(chosen.alternatives[0].name, "pizza-delivery");
        let fallback = break_tie(&top, None);
        assert_eq!(fallback.path, "/p/pizza-delivery");
        assert!(fallback.is_low());
        assert_eq!(break_tie(&top, Some(9)).path, "/p/pizza-delivery");
    }

    fn names(plan: &[PlannedStage]) -> Vec<&str> {
        plan.iter().map(|s| s.name.as_str()).collect()
    }

    #[test]
    fn prompts_pick_the_matching_plan_template_with_a_reason_per_stage() {
        let feature = plan_for(classify("add a dark mode toggle"));
        assert_eq!(
            names(&feature),
            vec!["grill", "spec", "implement", "review", "ship"]
        );
        let bug = plan_for(classify("fix the crash when the queue is empty"));
        assert_eq!(names(&bug), vec!["grill", "implement", "review", "ship"]);
        let investigate = plan_for(classify("investigate why builds are slow and decide"));
        assert_eq!(
            names(&investigate),
            vec!["investigate", "grill", "implement", "review", "ship"]
        );
        assert!(feature.iter().all(|s| !s.reason.is_empty()));
        assert!(investigate.iter().all(|s| !s.reason.is_empty()));
    }

    #[test]
    fn risky_prompts_are_flagged() {
        assert_eq!(
            risk_flags("drop the users table in production"),
            vec!["destructive-change".to_owned(), "production".to_owned()]
        );
        assert!(risk_flags("rename a variable").is_empty());
    }

    #[test]
    fn triage_bundles_repo_plan_flags_and_a_restated_task() {
        let resolution = explicit("/r/app");
        let result = triage("  fix the   crash \n in production ", resolution.clone());
        assert_eq!(result.repo, resolution);
        assert_eq!(result.template, Template::BugFix);
        assert_eq!(result.risk_flags, vec!["production".to_owned()]);
        assert!(result.task.contains("fix the crash in production"));
        assert!(result.task.contains("/r/app"));
    }

    fn stage(name: &str) -> PlannedStage {
        PlannedStage {
            name: name.to_owned(),
            reason: "r".to_owned(),
        }
    }

    #[test]
    fn a_plan_may_be_amended_up_to_the_stage_bound() {
        let plan = plan_for(Template::BugFix);
        let amended = amend_plan(&plan, "implement", vec![stage("fix")], 5);
        assert_eq!(
            amended.as_ref().map(|p| names(p)),
            Ok(vec!["grill", "implement", "fix", "review", "ship"])
        );
        assert_eq!(
            amend_plan(&plan, "implement", vec![stage("a"), stage("b")], 5),
            Err(AmendError::ExceedsBound {
                max: 5,
                would_be: 6
            })
        );
        assert_eq!(
            amend_plan(&plan, "nope", vec![stage("a")], 5),
            Err(AmendError::UnknownStage("nope".to_owned()))
        );
    }

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: crate::id::JobId::new("J1"),
            stage_id: None,
            seq,
            ts_ms: 0,
            actor: crate::event::Actor::Daemon,
            payload,
        }
    }

    #[test]
    fn the_current_plan_folds_the_triage_plan_and_accepted_amendments_only() {
        let result = triage("fix it", explicit("/r"));
        let events = [
            ev(1, EventPayload::TriageCompleted { triage: result }),
            ev(
                2,
                EventPayload::PlanAmended {
                    after: "implement".into(),
                    stages: vec![stage("fix")],
                },
            ),
            ev(
                3,
                EventPayload::PlanAmended {
                    after: "missing".into(),
                    stages: vec![stage("ignored")],
                },
            ),
        ];
        assert_eq!(
            names(&current_plan(&events)),
            vec!["grill", "implement", "fix", "review", "ship"]
        );
        assert!(current_plan(&[]).is_empty());
    }

    #[test]
    fn a_missing_repo_ends_as_needs_human_with_a_feedback_entry() {
        let resolved = resolve_repo(None, "do something unrelated", &index(), &|_| None);
        assert_eq!(resolved, None);
        let outcome = conclude("do something unrelated", resolved);
        assert_eq!(outcome.triage, None);
        assert_eq!(outcome.feedback.len(), 1);
        assert_eq!(outcome.feedback[0].kind, "needs-human");
    }

    #[test]
    fn low_confidence_takes_the_best_candidate_and_records_an_assumption() {
        let resolved = resolve_repo(None, "fix the pizza ordering", &index(), &|_| None);
        let outcome = conclude("fix the pizza ordering", resolved);
        assert_eq!(
            outcome.triage.as_ref().map(|t| t.repo.path.as_str()),
            Some("/p/pizza-delivery")
        );
        assert_eq!(outcome.feedback.len(), 1);
        assert_eq!(outcome.feedback[0].kind, "assumption");
        assert!(outcome.feedback[0]
            .confidence
            .is_some_and(|c| c < LOW_CONFIDENCE));
    }

    #[test]
    fn a_confident_resolution_records_no_assumption() {
        let resolved = resolve_repo(None, "fix the pizza ordering", &index(), &|_| Some(1));
        let outcome = conclude("fix the pizza ordering", resolved);
        assert!(outcome.feedback.is_empty());
        assert_eq!(
            outcome.triage.as_ref().map(|t| t.repo.path.as_str()),
            Some("/p/pizza-web")
        );
    }

    #[test]
    fn the_model_reply_is_read_from_the_result_line_and_parsed_as_a_choice() {
        let stdout = "{\"type\":\"assistant\"}\n{\"type\":\"result\",\"result\":\"Option 2.\"}\n";
        assert_eq!(reply_text(stdout), "Option 2.");
        assert_eq!(parse_choice("Option 2.", 3), Some(1));
        assert_eq!(parse_choice("7", 3), None);
        assert_eq!(parse_choice("none", 3), None);
        let top = tied(rank(None, "fix the pizza ordering", &index()));
        assert!(tiebreak_prompt("fix", &top).contains("2. pizza-web (/p/pizza-web)"));
    }
}
