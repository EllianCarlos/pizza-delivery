use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::quota::QuotaSnapshot;

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
            AdapterKind::Opencode | AdapterKind::Hermes => &[Capability::Resume],
            AdapterKind::Agy => &[],
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingConfig {
    pub agents: BTreeMap<String, AgentDef>,
    pub rules: Vec<Rule>,
    pub threshold_permille: u32,
    pub default_backoff_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    #[error("routing config is not valid TOML for this schema: {0}")]
    Parse(String),
    #[error("quota_threshold must be above 0 and at most 1, got {0}")]
    Threshold(String),
    #[error("rule {rule} has an empty chain")]
    EmptyChain { rule: usize },
    #[error("rule {rule} names agent '{agent}' which is not defined under [agents]")]
    UnknownAgent { rule: usize, agent: String },
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
            threshold_permille: DEFAULT_THRESHOLD_PERMILLE,
            default_backoff_ms: DEFAULT_BACKOFF_MS,
        }
    }

    pub fn with_owner(self, text: &str) -> Result<Self, ConfigError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.message().to_owned()))?;
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
        let merged = Self {
            agents,
            rules,
            threshold_permille,
            default_backoff_ms,
        };
        merged.validate().map(|()| merged)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        self.rules.iter().enumerate().try_for_each(|(rule, r)| {
            if r.chain.is_empty() {
                return Err(ConfigError::EmptyChain { rule });
            }
            r.chain
                .iter()
                .find(|c| !self.agents.contains_key(&c.agent))
                .map_or(Ok(()), |c| {
                    Err(ConfigError::UnknownAgent {
                        rule,
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

pub fn route(
    config: &RoutingConfig,
    request: &RouteRequest<'_>,
    quota: &QuotaSnapshot,
    now_ms: i64,
) -> Result<Routed, RouteError> {
    let rule = config
        .rules
        .iter()
        .find(|r| rule_matches(r, request))
        .ok_or_else(|| RouteError::NoRule {
            stage: request.stage.to_owned(),
        })?;
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
                agent: "claude".into(),
                model: Some("sonnet".into())
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
[agents.agy]
adapter = "agy"
command = "agy"
[[rules]]
stage = "review"
chain = [{ agent = "agy" }]
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
}
