use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::triage::{Feedback, PlannedStage};

pub const DEFAULT_CAP_TOKENS: u64 = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SizeTier {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageEstimate {
    pub stage: String,
    pub tier: SizeTier,
    pub tokens: u64,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    pub stage: String,
    pub tier: SizeTier,
    pub agent: String,
    pub tokens: u64,
    pub wall_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Defaults {
    pub small: (u64, u64),
    pub medium: (u64, u64),
    pub large: (u64, u64),
}

impl Defaults {
    pub fn standard() -> Self {
        Self {
            small: (50_000, 120_000),
            medium: (300_000, 600_000),
            large: (1_000_000, 1_800_000),
        }
    }

    fn for_tier(&self, tier: SizeTier) -> (u64, u64) {
        match tier {
            SizeTier::Small => self.small,
            SizeTier::Medium => self.medium,
            SizeTier::Large => self.large,
        }
    }
}

pub fn tier_for(stage: &str) -> SizeTier {
    match stage {
        "implement" => SizeTier::Large,
        "ship" | "grill" => SizeTier::Small,
        _ => SizeTier::Medium,
    }
}

fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len() % 2 == 1 {
        return values.get(mid).copied();
    }
    let lo = values.get(mid.checked_sub(1)?).copied()?;
    let hi = values.get(mid).copied()?;
    Some(lo / 2 + hi / 2 + (lo % 2 + hi % 2) / 2)
}

pub fn estimate_stage(
    stage: &str,
    tier: SizeTier,
    agent: &str,
    history: &[Sample],
    defaults: &Defaults,
) -> StageEstimate {
    let matching: Vec<&Sample> = history
        .iter()
        .filter(|s| s.stage == stage && s.tier == tier && s.agent == agent)
        .collect();
    let (default_tokens, default_wall) = defaults.for_tier(tier);
    StageEstimate {
        stage: stage.to_owned(),
        tier,
        tokens: median(matching.iter().map(|s| s.tokens).collect()).unwrap_or(default_tokens),
        wall_ms: median(matching.iter().map(|s| s.wall_ms).collect()).unwrap_or(default_wall),
    }
}

pub fn estimate_plan(
    plan: &[PlannedStage],
    agent: &str,
    history: &[Sample],
    defaults: &Defaults,
) -> Vec<StageEstimate> {
    plan.iter()
        .map(|s| estimate_stage(&s.name, tier_for(&s.name), agent, history, defaults))
        .collect()
}

pub fn estimated_total(estimates: &[StageEstimate]) -> u64 {
    estimates.iter().map(|e| e.tokens).sum()
}

pub fn estimate_overrun(estimates: &[StageEstimate], cap: u64) -> Option<Feedback> {
    let total = estimated_total(estimates);
    (total > cap).then(|| Feedback {
        kind: "budget".to_owned(),
        severity: "high".to_owned(),
        text: format!(
            "estimated {total} tokens exceeds the job budget cap of {cap}; running anyway"
        ),
        confidence: None,
        alternatives: Vec::new(),
        impact: None,
    })
}

pub fn actual_total(events: &[Event]) -> u64 {
    events
        .iter()
        .map(|e| match &e.payload {
            EventPayload::StageBudgetRecorded {
                actual_tokens: Some(t),
                ..
            } => *t,
            _ => 0,
        })
        .sum()
}

pub fn actual_overrun(events: &[Event], cap: u64) -> Option<Feedback> {
    let total = actual_total(events);
    (total > cap).then(|| Feedback {
        kind: "budget".to_owned(),
        severity: "high".to_owned(),
        text: format!("actual usage of {total} tokens exceeds the job budget cap of {cap}"),
        confidence: None,
        alternatives: Vec::new(),
        impact: None,
    })
}

pub fn estimate_for(events: &[Event], stage: &str) -> Option<StageEstimate> {
    events.iter().rev().find_map(|e| match &e.payload {
        EventPayload::BudgetEstimated { stages } => {
            stages.iter().find(|s| s.stage == stage).cloned()
        }
        _ => None,
    })
}

pub fn samples(events: &[Event]) -> Vec<Sample> {
    events
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::StageBudgetRecorded {
                stage,
                tier,
                agent,
                actual_tokens: Some(tokens),
                actual_wall_ms: Some(wall_ms),
                ..
            } => Some(Sample {
                stage: stage.clone(),
                tier: *tier,
                agent: agent.clone(),
                tokens: *tokens,
                wall_ms: *wall_ms,
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::id::JobId;

    fn planned(names: &[&str]) -> Vec<PlannedStage> {
        names
            .iter()
            .map(|n| PlannedStage {
                name: (*n).to_owned(),
                reason: String::new(),
            })
            .collect()
    }

    fn sample(stage: &str, agent: &str, tokens: u64, wall_ms: u64) -> Sample {
        Sample {
            stage: stage.to_owned(),
            tier: tier_for(stage),
            agent: agent.to_owned(),
            tokens,
            wall_ms,
        }
    }

    fn ev(seq: u64, payload: EventPayload) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: JobId::new("J"),
            stage_id: None,
            seq,
            ts_ms: 0,
            actor: Actor::Daemon,
            payload,
        }
    }

    fn recorded(stage: &str, tokens: Option<u64>, wall: Option<u64>) -> EventPayload {
        EventPayload::StageBudgetRecorded {
            stage: stage.to_owned(),
            tier: tier_for(stage),
            agent: "claude".to_owned(),
            estimate_tokens: 1,
            estimate_wall_ms: 1,
            actual_tokens: tokens,
            actual_wall_ms: wall,
        }
    }

    #[test]
    fn cold_start_estimates_fall_back_to_the_tier_defaults() {
        let got = estimate_plan(
            &planned(&["grill", "implement"]),
            "claude",
            &[],
            &Defaults::standard(),
        );
        assert_eq!(
            got,
            vec![
                StageEstimate {
                    stage: "grill".into(),
                    tier: SizeTier::Small,
                    tokens: 50_000,
                    wall_ms: 120_000
                },
                StageEstimate {
                    stage: "implement".into(),
                    tier: SizeTier::Large,
                    tokens: 1_000_000,
                    wall_ms: 1_800_000
                },
            ]
        );
    }

    #[test]
    fn history_medians_replace_defaults_per_stage_tier_and_agent() {
        let history = vec![
            sample("implement", "claude", 100, 10),
            sample("implement", "claude", 300, 30),
            sample("implement", "claude", 200, 20),
            sample("implement", "other", 9_999, 9_999),
            sample("review", "claude", 7, 7),
        ];
        let got = estimate_stage(
            "implement",
            SizeTier::Large,
            "claude",
            &history,
            &Defaults::standard(),
        );
        assert_eq!((got.tokens, got.wall_ms), (200, 20));
    }

    #[test]
    fn an_even_count_of_samples_takes_the_mean_of_the_middle_pair() {
        let history = vec![
            sample("implement", "claude", 100, 10),
            sample("implement", "claude", 300, 30),
        ];
        let got = estimate_stage(
            "implement",
            SizeTier::Large,
            "claude",
            &history,
            &Defaults::standard(),
        );
        assert_eq!((got.tokens, got.wall_ms), (200, 20));
    }

    #[test]
    fn an_estimate_over_the_cap_yields_a_high_severity_feedback_entry() {
        let estimates = estimate_plan(
            &planned(&["implement"]),
            "claude",
            &[],
            &Defaults::standard(),
        );
        let over = estimate_overrun(&estimates, 500_000);
        assert_eq!(over.as_ref().map(|f| f.severity.as_str()), Some("high"));
        assert_eq!(estimate_overrun(&estimates, 1_000_000), None);
    }

    #[test]
    fn actual_usage_is_summed_across_recorded_stages_ignoring_unknowns() {
        let events = vec![
            ev(1, recorded("grill", Some(40), Some(1))),
            ev(2, recorded("implement", None, None)),
            ev(3, recorded("review", Some(70), Some(2))),
        ];
        assert_eq!(actual_total(&events), 110);
        assert!(actual_overrun(&events, 110).is_none());
        assert_eq!(
            actual_overrun(&events, 109).map(|f| f.severity),
            Some("high".to_owned())
        );
    }

    #[test]
    fn samples_come_only_from_fully_known_actuals() {
        let events = vec![
            ev(1, recorded("grill", Some(40), Some(1))),
            ev(2, recorded("implement", None, Some(5))),
        ];
        assert_eq!(samples(&events), vec![sample("grill", "claude", 40, 1)]);
    }

    #[test]
    fn the_estimate_for_a_stage_is_read_from_the_job_ledger() {
        let estimates = estimate_plan(
            &planned(&["implement"]),
            "claude",
            &[],
            &Defaults::standard(),
        );
        let events = vec![ev(
            1,
            EventPayload::BudgetEstimated {
                stages: estimates.clone(),
            },
        )];
        assert_eq!(
            estimate_for(&events, "implement"),
            estimates.first().cloned()
        );
        assert_eq!(estimate_for(&events, "review"), None);
    }
}
