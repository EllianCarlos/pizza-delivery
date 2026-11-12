use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::quota::QuotaSnapshot;
use crate::triage::Template;

pub const DEFAULT_THRESHOLD_PERMILLE: u32 = 900;
pub const DEFAULT_BACKOFF_MS: i64 = 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Skills,
    Resume,
    Subagents,
    StructuredQuota,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AdapterKind {
    Claude,
    Opencode,
    Hermes,
    Agy,
}

impl AdapterKind {
    pub fn capabilities(self) -> &'static [Capability] {
        match self {
            AdapterKind::Claude => &[
                Capability::Skills,
                Capability::Resume,
                Capability::Subagents,
                Capability::StructuredQuota,
            ],
            // The daemon renders each stage template into one self-contained
            // prompt, so a CLI that takes a prompt argument can follow it.
            AdapterKind::Opencode => &[Capability::Skills, Capability::Resume],
            AdapterKind::Agy => &[Capability::Skills],
            AdapterKind::Hermes => &[Capability::Resume],
        }
    }

    pub fn has(self, needed: &[Capability]) -> bool {
        needed.iter().all(|c| self.capabilities().contains(c))
    }
}

const SKILL_DRIVEN_STAGES: [&str; 5] = ["grill", "spec", "implement", "fix", "review"];

pub fn stage_capabilities(stage: &str) -> Vec<Capability> {
    if SKILL_DRIVEN_STAGES.contains(&stage) {
        vec![Capability::Skills]
    } else {
        vec![]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentDef {
    pub adapter: AdapterKind,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub requires: Vec<Capability>,
    pub chain: Vec<Candidate>,
}

/// One row of the owner's model decision table (`[[models]]` in
/// routing.toml). Every condition is optional; an unset condition matches
/// anything. The first row whose conditions all hold picks the model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRule {
    /// Stage name, e.g. "review".
    #[serde(default)]
    pub stage: Option<String>,
    /// Template triage chose: full-feature, bug-fix, investigate or report.
    #[serde(default)]
    pub template: Option<Template>,
    /// true matches jobs with any risk flag, false jobs with none.
    #[serde(default)]
    pub risky: Option<bool>,
    /// Matches jobs carrying this exact risk flag.
    #[serde(default)]
    pub risk_flag: Option<String>,
    /// Repository name or path suffix, as in [[rules]].
    #[serde(default)]
    pub repo: Option<String>,
    /// Agent the row applies to (a name under [agents]); default "claude".
    #[serde(default)]
    pub agent: Option<String>,
    pub model: String,
}

/// What the model decider looks at for one stage of one job.
#[derive(Debug, Clone, Copy, Default)]
pub struct ModelInputs<'a> {
    pub stage: &'a str,
    pub template: Option<Template>,
    pub risk_flags: &'a [String],
    pub repo: Option<&'a str>,
    /// The submitter's `model` option, which beats every rule.
    pub job_model: Option<&'a str>,
    /// The LLM decider's pick for this stage, set only for jobs in the live
    /// arm of the decider trial. Used after the owner's rules, before the
    /// built-in default, and only for Claude.
    pub decider: Option<&'a str>,
}

impl ModelRule {
    fn matches(&self, agent: &str, inputs: &ModelInputs<'_>) -> bool {
        self.agent.as_deref().unwrap_or("claude") == agent
            && self.stage.as_deref().map_or(true, |s| s == inputs.stage)
            && self.template.map_or(true, |t| Some(t) == inputs.template)
            && self
                .risky
                .map_or(true, |r| r == !inputs.risk_flags.is_empty())
            && self
                .risk_flag
                .as_deref()
                .map_or(true, |f| inputs.risk_flags.iter().any(|x| x == f))
            && self
                .repo
                .as_deref()
                .map_or(true, |p| inputs.repo.is_some_and(|r| repo_matches(p, r)))
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    quota_threshold: Option<f64>,
    #[serde(default)]
    default_backoff_secs: Option<u64>,
    #[serde(default)]
    agents: BTreeMap<String, AgentDef>,
    #[serde(default)]
    rules: Vec<Rule>,
    #[serde(default)]
    models: Vec<ModelRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingConfig {
    pub agents: BTreeMap<String, AgentDef>,
    pub rules: Vec<Rule>,
    pub models: Vec<ModelRule>,
    pub threshold_permille: u32,
    pub default_backoff_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("routing config is not valid for this schema: {0}")]
    Parse(String),
    #[error("quota_threshold must be above 0 and at most 1, got {0}")]
    Threshold(String),
    #[error("rules[{rule}].chain is empty")]
    EmptyChain { rule: usize },
    #[error("models[{row}].agent names agent '{agent}' which is not defined under [agents]")]
    UnknownModelAgent { row: usize, agent: String },
    #[error("rules[{rule}].chain[{index}].agent names agent '{agent}' which is not defined under [agents]")]
    UnknownAgent {
        rule: usize,
        index: usize,
        agent: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RouteError {
    #[error("no routing rule matches stage '{stage}'")]
    NoRule { stage: String },
    #[error("no agent in the chain for stage '{stage}' has the required capabilities")]
    NoCapableAgent { stage: String },
}

impl RoutingConfig {
    pub fn builtin(claude_command: &str) -> Self {
        Self {
            agents: BTreeMap::from([(
                "claude".to_owned(),
                AgentDef {
                    adapter: AdapterKind::Claude,
                    command: claude_command.to_owned(),
                },
            )]),
            rules: vec![Rule {
                stage: None,
                tier: None,
                repo: None,
                requires: vec![],
                chain: vec![Candidate {
                    agent: "claude".to_owned(),
                    model: None,
                }],
            }],
            models: vec![],
            threshold_permille: DEFAULT_THRESHOLD_PERMILLE,
            default_backoff_ms: DEFAULT_BACKOFF_MS,
        }
    }

    pub fn with_owner(self, text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| {
            let line = e.span().map_or(String::new(), |span| {
                let upto = text.get(..span.start).unwrap_or(text);
                format!("line {}: ", upto.matches('\n').count() + 1)
            });
            ConfigError::Parse(format!("{line}{}", e.message()))
        })?;
        let threshold_permille = match raw.quota_threshold {
            None => self.threshold_permille,
            Some(t) if t > 0.0 && t <= 1.0 => (t * 1000.0).round() as u32,
            Some(t) => return Err(ConfigError::Threshold(t.to_string())),
        };
        let default_backoff_ms = raw
            .default_backoff_secs
            .map_or(self.default_backoff_ms, |s| {
                i64::try_from(s.saturating_mul(1000)).unwrap_or(i64::MAX)
            });
        let agents: BTreeMap<String, AgentDef> =
            self.agents.into_iter().chain(raw.agents).collect();
        let rules: Vec<Rule> = raw.rules.into_iter().chain(self.rules).collect();
        let models: Vec<ModelRule> = raw.models.into_iter().chain(self.models).collect();
        let merged = Self {
            agents,
            rules,
            models,
            threshold_permille,
            default_backoff_ms,
        };
        merged.validate().map(|()| merged)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if let Some((row, agent)) = self.models.iter().enumerate().find_map(|(row, m)| {
            let agent = m.agent.as_deref().unwrap_or("claude");
            (!self.agents.contains_key(agent)).then(|| (row, agent.to_owned()))
        }) {
            return Err(ConfigError::UnknownModelAgent { row, agent });
        }
        self.rules.iter().enumerate().try_for_each(|(rule, r)| {
            if r.chain.is_empty() {
                return Err(ConfigError::EmptyChain { rule });
            }
            r.chain
                .iter()
                .enumerate()
                .find(|(_, c)| !self.agents.contains_key(&c.agent))
                .map_or(Ok(()), |(index, c)| {
                    Err(ConfigError::UnknownAgent {
                        rule,
                        index,
                        agent: c.agent.clone(),
                    })
                })
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RouteRequest<'a> {
    pub stage: &'a str,
    pub tier: &'a str,
    pub repo: Option<&'a str>,
    pub required: &'a [Capability],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Skip {
    Exhausted { until_ms: i64 },
    Utilization { resets_at_ms: Option<i64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routed {
    pub candidates: Vec<Candidate>,
    pub skipped: Vec<(Candidate, Skip)>,
}

fn repo_matches(pattern: &str, repo: &str) -> bool {
    let trimmed = repo.trim_end_matches('/');
    trimmed == pattern || trimmed.ends_with(&format!("/{}", pattern.trim_start_matches('/')))
}

fn rule_matches(rule: &Rule, request: &RouteRequest<'_>) -> bool {
    rule.stage.as_deref().map_or(true, |s| s == request.stage)
        && rule.tier.as_deref().map_or(true, |t| t == request.tier)
        && rule
            .repo
            .as_deref()
            .map_or(true, |p| request.repo.is_some_and(|r| repo_matches(p, r)))
        && rule.requires.iter().all(|c| request.required.contains(c))
}

fn skip_for(
    config: &RoutingConfig,
    quota: &QuotaSnapshot,
    candidate: &Candidate,
    now_ms: i64,
) -> Option<Skip> {
    let q = quota.get(&candidate.agent)?;
    if let Some(until_ms) = q.exhausted_until_ms.filter(|until| *until > now_ms) {
        return Some(Skip::Exhausted { until_ms });
    }
    let hot = q
        .utilization_permille
        .is_some_and(|u| u >= config.threshold_permille);
    let still_in_window = q.resets_at_ms.map_or(true, |reset| reset > now_ms);
    (hot && still_in_window).then_some(Skip::Utilization {
        resets_at_ms: q.resets_at_ms,
    })
}

fn matching_rule<'a>(
    config: &'a RoutingConfig,
    request: &RouteRequest<'_>,
) -> Result<(usize, &'a Rule), RouteError> {
    config
        .rules
        .iter()
        .enumerate()
        .find(|(_, r)| rule_matches(r, request))
        .ok_or_else(|| RouteError::NoRule {
            stage: request.stage.to_owned(),
        })
}

pub fn route(
    config: &RoutingConfig,
    request: &RouteRequest<'_>,
    quota: &QuotaSnapshot,
    now_ms: i64,
) -> Result<Routed, RouteError> {
    let (_, rule) = matching_rule(config, request)?;
    let capable: Vec<&Candidate> = rule
        .chain
        .iter()
        .filter(|c| {
            config
                .agents
                .get(&c.agent)
                .is_some_and(|a| a.adapter.has(request.required))
        })
        .collect();
    if capable.is_empty() {
        return Err(RouteError::NoCapableAgent {
            stage: request.stage.to_owned(),
        });
    }
    let (candidates, skipped) = capable.into_iter().fold(
        (Vec::new(), Vec::new()),
        |(mut ok, mut skipped), candidate| {
            match skip_for(config, quota, candidate, now_ms) {
                Some(skip) => skipped.push((candidate.clone(), skip)),
                None => ok.push(candidate.clone()),
            }
            (ok, skipped)
        },
    );
    Ok(Routed {
        candidates,
        skipped,
    })
}

/// The model Claude runs a stage on when neither the job nor the routing rule
/// names one. Without this the CLI falls back to the owner's own default,
/// which can be Opus. Opus is never chosen here: it needs an explicit pick.
pub fn default_claude_model(stage: &str) -> &'static str {
    match stage {
        "triage" | "memorize" => "haiku",
        _ => "sonnet",
    }
}

/// Fills in the model each candidate runs on: the rules rung of the model
/// decider. Precedence, highest first:
/// 1. the job's own `model` option (Claude candidates only, since model
///    names are per-provider);
/// 2. a model the [[rules]] chain names for that candidate;
/// 3. the first matching row of the owner's [[models]] table;
/// 4. for Claude, the LLM decider's pick in the live arm of its trial;
/// 5. for Claude, the stage default (never Opus). Other agents stay unset,
///    so their CLI picks its own default.
pub fn resolve_models(config: &RoutingConfig, routed: Routed, inputs: &ModelInputs<'_>) -> Routed {
    let is_claude = |c: &Candidate| {
        config
            .agents
            .get(&c.agent)
            .is_some_and(|a| a.adapter == AdapterKind::Claude)
    };
    let resolve = |c: Candidate| {
        let claude = is_claude(&c);
        let model = inputs
            .job_model
            .filter(|_| claude)
            .map(str::to_owned)
            .or(c.model)
            .or_else(|| {
                config
                    .models
                    .iter()
                    .find(|m| m.matches(&c.agent, inputs))
                    .map(|m| m.model.clone())
            })
            .or_else(|| inputs.decider.filter(|_| claude).map(str::to_owned))
            .or_else(|| claude.then(|| default_claude_model(inputs.stage).to_owned()));
        Candidate {
            agent: c.agent,
            model,
        }
    };
    Routed {
        candidates: routed.candidates.into_iter().map(resolve).collect(),
        skipped: routed
            .skipped
            .into_iter()
            .map(|(c, s)| (resolve(c), s))
            .collect(),
    }
}

/// Stages the daemon routes, in pipeline order, plus the two cheap ones.
pub const CHECKED_STAGES: [&str; 8] = [
    "triage",
    "grill",
    "spec",
    "investigate",
    "implement",
    "fix",
    "review",
    "memorize",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedCandidate {
    pub candidate: Candidate,
    pub adapter: AdapterKind,
    /// False when the adapter lacks a capability the stage needs, so the
    /// router would skip it.
    pub usable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageCheck {
    pub stage: String,
    pub required: Vec<Capability>,
    pub rule: Option<usize>,
    pub chain: Vec<CheckedCandidate>,
    pub problem: Option<RouteError>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub threshold_permille: u32,
    pub default_backoff_ms: i64,
    pub stages: Vec<StageCheck>,
}

impl CheckReport {
    pub fn is_ok(&self) -> bool {
        self.stages.iter().all(|s| s.problem.is_none())
    }
}

/// Resolve each routed stage against the rules (no repo, standard tier) the
/// way the daemon would, without any quota state.
pub fn check(config: &RoutingConfig) -> CheckReport {
    let stages = CHECKED_STAGES
        .iter()
        .map(|stage| {
            let required = stage_capabilities(stage);
            let request = RouteRequest {
                stage,
                tier: "standard",
                repo: None,
                required: &required,
            };
            match matching_rule(config, &request) {
                Err(problem) => StageCheck {
                    stage: (*stage).to_owned(),
                    required,
                    rule: None,
                    chain: Vec::new(),
                    problem: Some(problem),
                },
                Ok((rule, r)) => {
                    let chain: Vec<CheckedCandidate> = r
                        .chain
                        .iter()
                        .filter_map(|c| {
                            config.agents.get(&c.agent).map(|a| CheckedCandidate {
                                candidate: c.clone(),
                                adapter: a.adapter,
                                usable: a.adapter.has(&required),
                            })
                        })
                        .collect();
                    let problem =
                        (!chain.iter().any(|c| c.usable)).then(|| RouteError::NoCapableAgent {
                            stage: (*stage).to_owned(),
                        });
                    StageCheck {
                        stage: (*stage).to_owned(),
                        required,
                        rule: Some(rule),
                        chain,
                        problem,
                    }
                }
            }
        })
        .collect();
    CheckReport {
        threshold_permille: config.threshold_permille,
        default_backoff_ms: config.default_backoff_ms,
        stages,
    }
}

/// Arguments of a cheap, side-effect-free "list models" command, when the
/// adapter's CLI has one.
pub fn model_list_args(kind: AdapterKind) -> Option<&'static [&'static str]> {
    match kind {
        AdapterKind::Opencode | AdapterKind::Agy => Some(&["models"]),
        AdapterKind::Claude | AdapterKind::Hermes => None,
    }
}

/// First whitespace-separated token of each line, skipping progress lines.
pub fn parse_model_list(text: &str) -> Vec<String> {
    text.lines()
        .filter(|l| !l.trim_end().ends_with("..."))
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_owned)
        .collect()
}

/// A model matches a listed one ignoring an opencode `#variant` suffix.
pub fn model_known(listed: &[String], model: &str) -> bool {
    let base = model.split('#').next().unwrap_or(model);
    listed.iter().any(|m| m == model || m == base)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Actor, Event, EventPayload};
    use crate::id::JobId;
    use crate::quota::{fold_quota, QuotaStatus};

    const OWNER: &str = r#"
quota_threshold = 0.8
default_backoff_secs = 60

[agents.opencode]
adapter = "opencode"
command = "opencode"

[agents.agy]
adapter = "agy"
command = "agy"

[[rules]]
stage = "triage"
chain = [
  { agent = "agy", model = "gemini-flash" },
  { agent = "opencode", model = "deepseek/chat" },
  { agent = "claude", model = "haiku" },
]

[[rules]]
stage = "spec"
tier = "premium"
repo = "big-repo"
chain = [
  { agent = "claude", model = "opus" },
  { agent = "opencode", model = "x/y" },
]

[[rules]]
requires = ["skills"]
chain = [
  { agent = "opencode" },
  { agent = "claude", model = "sonnet" },
]
"#;

    fn config() -> RoutingConfig {
        RoutingConfig::builtin("claude")
            .with_owner(OWNER)
            .unwrap_or_else(|_| RoutingConfig::builtin("claude"))
    }

    fn agents(routed: &Routed) -> Vec<&str> {
        routed.candidates.iter().map(|c| c.agent.as_str()).collect()
    }

    fn req<'a>(stage: &'a str, required: &'a [Capability]) -> RouteRequest<'a> {
        RouteRequest {
            stage,
            tier: "standard",
            repo: Some("/home/u/src/my-repo"),
            required,
        }
    }

    fn signal(
        agent: &str,
        status: QuotaStatus,
        utilization: Option<u32>,
        resets: Option<i64>,
    ) -> Event {
        Event {
            id: "e".into(),
            job_id: JobId::new("J"),
            stage_id: None,
            seq: 1,
            ts_ms: 0,
            actor: Actor::Daemon,
            payload: EventPayload::QuotaSignal {
                agent: agent.to_owned(),
                model: None,
                status,
                utilization_permille: utilization,
                resets_at_ms: resets,
                reset_known: resets.is_some(),
            },
        }
    }

    #[test]
    fn owner_rules_override_defaults_and_the_first_matching_rule_wins() {
        let routed = route(&config(), &req("triage", &[]), &QuotaSnapshot::default(), 0);
        assert_eq!(
            routed.map(|r| r
                .candidates
                .iter()
                .map(|c| (c.agent.clone(), c.model.clone()))
                .collect::<Vec<_>>()),
            Ok(vec![
                ("agy".to_owned(), Some("gemini-flash".to_owned())),
                ("opencode".to_owned(), Some("deepseek/chat".to_owned())),
                ("claude".to_owned(), Some("haiku".to_owned())),
            ])
        );
    }

    #[test]
    fn rules_match_on_tier_and_repo() {
        let premium = RouteRequest {
            tier: "premium",
            repo: Some("/src/big-repo"),
            ..req("spec", &[Capability::Skills])
        };
        let other_repo = RouteRequest {
            tier: "premium",
            ..req("spec", &[Capability::Skills])
        };
        let quota = QuotaSnapshot::default();
        assert_eq!(
            route(&config(), &premium, &quota, 0).map(|r| r.candidates.first().cloned()),
            Ok(Some(Candidate {
                agent: "claude".into(),
                model: Some("opus".into())
            }))
        );
        assert_eq!(
            route(&config(), &other_repo, &quota, 0).map(|r| r.candidates.first().cloned()),
            Ok(Some(Candidate {
                agent: "opencode".into(),
                model: None
            }))
        );
    }

    #[test]
    fn skill_driven_stages_route_only_to_skill_capable_adapters() {
        for stage in ["grill", "spec", "implement", "fix", "review"] {
            let needs = stage_capabilities(stage);
            let routed = route(&config(), &req(stage, &needs), &QuotaSnapshot::default(), 0);
            assert_eq!(needs, vec![Capability::Skills], "{stage}");
            let candidates = routed.map(|r| r.candidates).unwrap_or_default();
            assert!(!candidates.is_empty(), "{stage}");
            assert!(
                candidates.iter().all(|c| config()
                    .agents
                    .get(&c.agent)
                    .is_some_and(|a| a.adapter.has(&needs))),
                "{stage}"
            );
        }
        for stage in ["triage", "investigate"] {
            assert_eq!(stage_capabilities(stage), vec![], "{stage}");
        }
    }

    #[test]
    fn cheap_stages_may_use_any_adapter() {
        assert_eq!(stage_capabilities("triage"), vec![]);
        let routed = route(&config(), &req("triage", &[]), &QuotaSnapshot::default(), 0);
        assert_eq!(
            routed.as_ref().map(agents),
            Ok(vec!["agy", "opencode", "claude"])
        );
    }

    #[test]
    fn a_chain_with_no_capable_agent_is_a_routing_error() {
        let only_cheap = RoutingConfig::builtin("claude").with_owner(
            r#"
[agents.hermes]
adapter = "hermes"
command = "hermes"
[[rules]]
stage = "review"
chain = [{ agent = "hermes" }]
"#,
        );
        let routed = only_cheap.map(|c| {
            route(
                &c,
                &req("review", &[Capability::Skills]),
                &QuotaSnapshot::default(),
                0,
            )
        });
        assert_eq!(
            routed,
            Ok(Err(RouteError::NoCapableAgent {
                stage: "review".into()
            }))
        );
    }

    #[test]
    fn exhausted_agents_are_skipped_until_their_reset() {
        let quota = fold_quota(&[signal("agy", QuotaStatus::Exhausted, None, Some(1_000))]);
        let during = route(&config(), &req("triage", &[]), &quota, 500);
        let after = route(&config(), &req("triage", &[]), &quota, 1_000);
        assert_eq!(during.as_ref().map(agents), Ok(vec!["opencode", "claude"]));
        assert_eq!(
            during.map(|r| r.skipped.first().map(|(_, s)| s.clone())),
            Ok(Some(Skip::Exhausted { until_ms: 1_000 }))
        );
        assert_eq!(
            after.as_ref().map(agents),
            Ok(vec!["agy", "opencode", "claude"])
        );
    }

    #[test]
    fn a_candidate_above_the_utilization_threshold_is_skipped_proactively() {
        let hot = fold_quota(&[signal(
            "claude",
            QuotaStatus::Allowed,
            Some(850),
            Some(5_000),
        )]);
        let cool = fold_quota(&[signal(
            "claude",
            QuotaStatus::Allowed,
            Some(790),
            Some(5_000),
        )]);
        let stale = fold_quota(&[signal("claude", QuotaStatus::Allowed, Some(990), Some(100))]);
        let request = req("triage", &[]);
        assert_eq!(
            route(&config(), &request, &hot, 10).as_ref().map(agents),
            Ok(vec!["agy", "opencode"])
        );
        assert_eq!(
            route(&config(), &request, &cool, 10).as_ref().map(agents),
            Ok(vec!["agy", "opencode", "claude"])
        );
        assert_eq!(
            route(&config(), &request, &stale, 200).as_ref().map(agents),
            Ok(vec!["agy", "opencode", "claude"])
        );
    }

    #[test]
    fn with_everything_exhausted_nothing_is_offered_but_the_skips_say_when_to_retry() {
        let quota = fold_quota(&[signal("claude", QuotaStatus::Exhausted, None, Some(7_000))]);
        let routed = route(&config(), &req("implement", &[]), &quota, 0);
        assert_eq!(
            routed.map(|r| (r.candidates.len(), r.skipped.len())),
            Ok((0, 1))
        );
    }

    #[test]
    fn a_stage_with_no_matching_rule_is_a_routing_error() {
        let mut cfg = config();
        cfg.rules.retain(|r| r.stage.is_some());
        assert_eq!(
            route(&cfg, &req("implement", &[]), &QuotaSnapshot::default(), 0),
            Err(RouteError::NoRule {
                stage: "implement".into()
            })
        );
    }

    #[test]
    fn config_errors_are_reported_not_defaulted() {
        let base = || RoutingConfig::builtin("claude");
        assert!(matches!(
            base().with_owner("[[rules]]\nchain = [{ agent = \"ghost\" }]"),
            Err(ConfigError::UnknownAgent { .. })
        ));
        assert!(matches!(
            base().with_owner("[[rules]]\nchain = []"),
            Err(ConfigError::EmptyChain { .. })
        ));
        assert!(matches!(
            base().with_owner("quota_threshold = 1.5"),
            Err(ConfigError::Threshold(_))
        ));
        assert!(matches!(
            base().with_owner("[agents.x]\nadapter = \"nope\"\ncommand = \"x\""),
            Err(ConfigError::Parse(_))
        ));
        assert!(matches!(
            base().with_owner("surprise = true"),
            Err(ConfigError::Parse(_))
        ));
    }

    #[test]
    fn owner_threshold_and_backoff_override_the_defaults() {
        let cfg = config();
        assert_eq!(
            (cfg.threshold_permille, cfg.default_backoff_ms),
            (800, 60_000)
        );
        let builtin = RoutingConfig::builtin("claude");
        assert_eq!(
            (builtin.threshold_permille, builtin.default_backoff_ms),
            (DEFAULT_THRESHOLD_PERMILLE, DEFAULT_BACKOFF_MS)
        );
    }

    #[test]
    fn opencode_and_agy_follow_the_inlined_stage_prompt_so_they_run_skill_stages() {
        assert!(AdapterKind::Opencode.has(&[Capability::Skills]));
        assert!(AdapterKind::Agy.has(&[Capability::Skills]));
        assert!(!AdapterKind::Hermes.has(&[Capability::Skills]));
        assert!(!AdapterKind::Opencode.has(&[Capability::Subagents]));
        assert!(!AdapterKind::Agy.has(&[Capability::Subagents]));
    }

    #[test]
    fn config_errors_name_the_field_at_fault() {
        let base = || RoutingConfig::builtin("claude");
        let msg = |text: &str| base().with_owner(text).map_err(|e| e.to_string());
        assert_eq!(
            msg("[[rules]]\nchain = [{ agent = \"claude\" }]\n[[rules]]\nchain = [{ agent = \"claude\" }, { agent = \"ghost\" }]"),
            Err("rules[1].chain[1].agent names agent 'ghost' which is not defined under [agents]".to_owned())
        );
        assert_eq!(
            msg("[[rules]]\nchain = []"),
            Err("rules[0].chain is empty".to_owned())
        );
        let parse = msg("\n\nsurprise = true").err().unwrap_or_default();
        assert!(
            parse.contains("line 3") && parse.contains("surprise"),
            "{parse}"
        );
    }

    #[test]
    fn check_resolves_every_stage_to_its_chain_and_flags_unusable_candidates() {
        let cfg = config();
        let report = check(&cfg);
        assert_eq!(report.threshold_permille, 800);
        let stage = |n: &str| report.stages.iter().find(|s| s.stage == n);
        let triage = stage("triage");
        assert_eq!(
            triage.map(|s| s
                .chain
                .iter()
                .map(|c| c.candidate.agent.as_str())
                .collect::<Vec<_>>()),
            Some(vec!["agy", "opencode", "claude"])
        );
        assert!(triage.is_some_and(|s| s.problem.is_none()));
        for name in [
            "grill",
            "spec",
            "implement",
            "fix",
            "review",
            "investigate",
            "memorize",
        ] {
            assert!(stage(name).is_some(), "{name}");
        }
        assert!(report.is_ok());
    }

    #[test]
    fn check_flags_a_skill_stage_whose_chain_has_no_skills_capable_agent() {
        let cfg = RoutingConfig::builtin("claude")
            .with_owner(
                r#"
[agents.hermes]
adapter = "hermes"
command = "hermes"
[[rules]]
stage = "review"
chain = [{ agent = "hermes" }]
"#,
            )
            .map_err(|e| e.to_string());
        let report = cfg.map(|c| check(&c));
        let review = report
            .as_ref()
            .ok()
            .and_then(|r| r.stages.iter().find(|s| s.stage == "review"));
        assert_eq!(
            review.map(|s| s.problem.clone()),
            Some(Some(RouteError::NoCapableAgent {
                stage: "review".to_owned()
            }))
        );
        assert!(review.is_some_and(|s| s.chain.iter().all(|c| !c.usable)));
        assert!(report.is_ok_and(|r| !r.is_ok()));
    }

    #[test]
    fn model_lists_parse_per_adapter_and_only_cheap_listers_are_probed() {
        assert_eq!(
            model_list_args(AdapterKind::Opencode),
            Some(&["models"][..])
        );
        assert_eq!(model_list_args(AdapterKind::Agy), Some(&["models"][..]));
        assert_eq!(model_list_args(AdapterKind::Claude), None);
        assert_eq!(model_list_args(AdapterKind::Hermes), None);
        assert_eq!(
            parse_model_list("Fetching available models...\ngemini-a-high\tGemini A (High)\n"),
            vec!["gemini-a-high"]
        );
        assert!(model_known(&["x/y".to_owned()], "x/y#fast"));
        assert!(!model_known(&["x/y".to_owned()], "x/z"));
    }

    #[test]
    fn claude_never_runs_without_a_model_and_the_job_model_wins() {
        let builtin = RoutingConfig::builtin("claude");
        let routed = |stage: &str, job: Option<&str>| {
            let required = stage_capabilities(stage);
            let r = route(
                &builtin,
                &RouteRequest {
                    stage,
                    tier: "standard",
                    repo: None,
                    required: &required,
                },
                &fold_quota(&[]),
                0,
            )
            .map(|r| {
                resolve_models(
                    &builtin,
                    r,
                    &ModelInputs {
                        stage,
                        job_model: job,
                        ..ModelInputs::default()
                    },
                )
            });
            r.ok()
                .and_then(|r| r.candidates.first().and_then(|c| c.model.clone()))
        };
        assert_eq!(routed("implement", None).as_deref(), Some("sonnet"));
        assert_eq!(routed("review", None).as_deref(), Some("sonnet"));
        assert_eq!(routed("triage", None).as_deref(), Some("haiku"));
        assert_eq!(routed("implement", Some("haiku")).as_deref(), Some("haiku"));
        for stage in CHECKED_STAGES {
            assert!(!default_claude_model(stage).contains("opus"), "{stage}");
        }
    }

    #[test]
    fn non_claude_candidates_keep_their_own_model() {
        let config = config();
        let required = stage_capabilities("triage");
        let routed = route(
            &config,
            &RouteRequest {
                stage: "triage",
                tier: "standard",
                repo: None,
                required: &required,
            },
            &fold_quota(&[]),
            0,
        )
        .map(|r| {
            resolve_models(
                &config,
                r,
                &ModelInputs {
                    stage: "triage",
                    job_model: Some("sonnet"),
                    ..ModelInputs::default()
                },
            )
        });
        let models: Vec<_> = routed
            .map(|r| {
                r.candidates
                    .into_iter()
                    .map(|c| (c.agent, c.model))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            models,
            vec![
                ("agy".to_owned(), Some("gemini-flash".to_owned())),
                ("opencode".to_owned(), Some("deepseek/chat".to_owned())),
                ("claude".to_owned(), Some("sonnet".to_owned())),
            ]
        );
    }

    #[test]
    fn the_owner_model_table_picks_by_stage_template_and_risk() {
        let owner = r#"
[[models]]
stage = "review"
risky = false
model = "haiku"

[[models]]
template = "investigate"
model = "haiku"

[[models]]
risk_flag = "production"
model = "claude-opus-5-5"
"#;
        let config = RoutingConfig::builtin("claude")
            .with_owner(owner)
            .unwrap_or_else(|_| RoutingConfig::builtin("broken"));
        assert_eq!(config.models.len(), 3);
        let pick = |stage: &str, template: Option<Template>, flags: &[String]| {
            let required = stage_capabilities(stage);
            route(
                &config,
                &RouteRequest {
                    stage,
                    tier: "standard",
                    repo: None,
                    required: &required,
                },
                &fold_quota(&[]),
                0,
            )
            .ok()
            .map(|r| {
                resolve_models(
                    &config,
                    r,
                    &ModelInputs {
                        stage,
                        template,
                        risk_flags: flags,
                        ..ModelInputs::default()
                    },
                )
            })
            .and_then(|r| r.candidates.first().and_then(|c| c.model.clone()))
        };
        let prod = vec!["production".to_owned()];
        assert_eq!(pick("review", None, &[]).as_deref(), Some("haiku"));
        assert_eq!(
            pick("review", None, &prod).as_deref(),
            Some("claude-opus-5-5")
        );
        assert_eq!(
            pick("implement", Some(Template::Investigate), &[]).as_deref(),
            Some("haiku")
        );
        assert_eq!(
            pick("implement", Some(Template::BugFix), &[]).as_deref(),
            Some("sonnet")
        );
    }

    #[test]
    fn a_model_row_for_an_undefined_agent_is_a_config_error() {
        let err = RoutingConfig::builtin("claude")
            .with_owner("[[models]]\nagent = \"nope\"\nmodel = \"x\"\n")
            .err();
        assert_eq!(
            err,
            Some(ConfigError::UnknownModelAgent {
                row: 0,
                agent: "nope".to_owned()
            })
        );
    }
}
