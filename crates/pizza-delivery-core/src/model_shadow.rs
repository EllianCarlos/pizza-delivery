//! Trial of an LLM model decider. A local model (for example Qwen on Ollama)
//! is asked, once per job before its first agent stage, which model each
//! stage should use, given only what triage knew.
//!
//! Jobs are split into two arms by a stable hash of the job id. In the live
//! arm (only jobs with no risk flags) the decider's pick actually runs,
//! limited to haiku or sonnet. In the shadow arm the pick is only recorded.
//! At the end of every job its picks, the models that ran, their cost and the
//! outcome are recorded, so the arms can be compared on cost and on whether
//! the job passed.

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::triage::Triage;

/// SOP used when the owner has not written one.
pub const DEFAULT_SOP: &str = "\
Pick the cheapest model that can finish each stage.
1. Default to sonnet for grill, spec, implement, fix and review.
2. Use haiku when the job is one file, adds no new behaviour and has no risk flags.
3. Never pick opus unless the job has risk flags.
4. If unsure between two models, pick the cheaper one.";

/// The decider's answer for one job, fixed before its first agent stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub decider: String,
    /// "live" when the picks run, "shadow" when they are only recorded.
    pub arm: String,
    /// Why the job is in its arm, or why the decider gave no answer.
    pub why: String,
    pub picks: Vec<ShadowPick>,
}

impl Decision {
    /// The model to run for a stage: only in the live arm, and only a model
    /// the trial allows (haiku or sonnet; never opus).
    pub fn live_model(&self, stage: &str) -> Option<&'static str> {
        if self.arm != "live" {
            return None;
        }
        let pick = self.picks.iter().find(|p| p.stage == stage)?;
        ["haiku", "sonnet"]
            .into_iter()
            .find(|f| pick.model.contains(f))
    }
}

/// Stable 0..100 bucket for a job id (FNV-1a), so a job keeps its arm.
pub fn bucket(job_id: &str) -> u8 {
    let hash = job_id.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    });
    u8::try_from(hash % 100).unwrap_or(0)
}

/// Which arm a job goes in, and why.
pub fn arm_for(job_id: &str, risk_flags: &[String], live_percent: u8) -> (&'static str, String) {
    if !risk_flags.is_empty() {
        return ("shadow", format!("risk flags: {}", risk_flags.join(", ")));
    }
    let b = bucket(job_id);
    if b < live_percent {
        (
            "live",
            format!("bucket {b} is below the live share of {live_percent}%"),
        )
    } else {
        (
            "shadow",
            format!("bucket {b} is the control group (live share {live_percent}%)"),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowPick {
    pub stage: String,
    pub model: String,
    #[serde(default)]
    pub reason: String,
}

/// One stage as the shadow record shows it: what the pilot picked and what ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowRow {
    pub stage: String,
    pub picked: Option<String>,
    pub reason: Option<String>,
    pub actual: Option<String>,
    pub agrees: bool,
    pub cost_microusd: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowRecord {
    pub decider: String,
    pub arm: String,
    pub cost_microusd: u64,
    pub rows: Vec<ShadowRow>,
    pub agreed: usize,
    pub compared: usize,
    pub outcome: String,
}

/// The prompt: the SOP plus only what was known at triage time.
pub fn shadow_prompt(sop: &str, triage: &Triage) -> String {
    let stages: Vec<&str> = triage.plan.iter().map(|s| s.name.as_str()).collect();
    let template = serde_json::to_string(&triage.template).unwrap_or_default();
    let flags = if triage.risk_flags.is_empty() {
        "none".to_owned()
    } else {
        triage.risk_flags.join(", ")
    };
    format!(
        "You choose which Claude model runs each stage of a coding job.\n\
         Allowed models: haiku, sonnet, opus.\n\n\
         Standard operating procedure:\n{sop}\n\n\
         Job task:\n{task}\n\n\
         Template: {template}\nRisk flags: {flags}\nStages: {stages}\n\n\
         Answer with JSON only, in this shape:\n\
         {{\"stages\": [{{\"stage\": \"<name>\", \"model\": \"<haiku|sonnet|opus>\", \"reason\": \"<one line>\"}}]}}",
        task = triage.task.trim(),
        stages = stages.join(", "),
    )
}

/// Reads the model's answer, tolerating text around the JSON object and
/// either a `stages` list or a `{stage: model}` map.
pub fn parse_picks(text: &str) -> Vec<ShadowPick> {
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return vec![];
    };
    let Some(json) = text.get(start..=end) else {
        return vec![];
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
        return vec![];
    };
    let normal = |m: &str| m.trim().to_lowercase();
    if let Some(list) = value.get("stages").and_then(|s| s.as_array()) {
        return list
            .iter()
            .filter_map(|p| serde_json::from_value::<ShadowPick>(p.clone()).ok())
            .map(|p| ShadowPick {
                model: normal(&p.model),
                ..p
            })
            .collect();
    }
    let map = value
        .get("stages")
        .and_then(|s| s.as_object())
        .or_else(|| value.as_object());
    map.map(|m| {
        m.iter()
            .filter_map(|(stage, model)| {
                model.as_str().map(|model| ShadowPick {
                    stage: stage.clone(),
                    model: normal(model),
                    reason: String::new(),
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Model family for comparison: "claude-sonnet-5-5" and "sonnet" agree.
fn family(model: &str) -> &str {
    ["haiku", "sonnet", "opus"]
        .into_iter()
        .find(|f| model.contains(f))
        .unwrap_or(model)
}

/// One stage as it actually ran: its model and what it cost in total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub stage: String,
    pub model: String,
    pub cost_microusd: Option<u64>,
}

/// The model each stage last ran on, and its cost summed over attempts.
pub fn actual_runs(events: &[Event]) -> Vec<Ran> {
    let stage_of = |id: &str| {
        events.iter().find_map(|e| match &e.payload {
            EventPayload::StagePlanned { name, .. } if e.stage_id.as_deref() == Some(id) => {
                Some(name.clone())
            }
            _ => None,
        })
    };
    let mut out: Vec<Ran> = vec![];
    for e in events {
        let EventPayload::AgentFinished { usage } = &e.payload else {
            continue;
        };
        let Some(stage) = e.stage_id.as_deref().and_then(stage_of) else {
            continue;
        };
        let earlier = out
            .iter()
            .position(|r| r.stage == stage)
            .map(|i| out.remove(i));
        let cost = match (
            earlier.as_ref().and_then(|r| r.cost_microusd),
            usage.cost_microusd,
        ) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
        };
        let model = usage
            .model
            .clone()
            .or_else(|| earlier.map(|r| r.model))
            .unwrap_or_default();
        out.push(Ran {
            stage,
            model,
            cost_microusd: cost,
        });
    }
    out
}

pub fn compare(
    decision: &Decision,
    plan: &[String],
    actual: &[Ran],
    outcome: &str,
) -> ShadowRecord {
    let rows: Vec<ShadowRow> = plan
        .iter()
        .map(|stage| {
            let pick = decision.picks.iter().find(|p| &p.stage == stage);
            let ran = actual.iter().find(|r| &r.stage == stage);
            let agrees = match (pick, ran) {
                (Some(p), Some(r)) => family(&p.model) == family(&r.model),
                _ => false,
            };
            ShadowRow {
                stage: stage.clone(),
                picked: pick.map(|p| p.model.clone()),
                reason: pick.map(|p| p.reason.clone()).filter(|r| !r.is_empty()),
                actual: ran.map(|r| r.model.clone()),
                agrees,
                cost_microusd: ran.and_then(|r| r.cost_microusd),
            }
        })
        .collect();
    let compared = rows
        .iter()
        .filter(|r| r.picked.is_some() && r.actual.is_some())
        .count();
    let agreed = rows.iter().filter(|r| r.agrees).count();
    ShadowRecord {
        decider: decision.decider.clone(),
        arm: decision.arm.clone(),
        cost_microusd: actual.iter().filter_map(|r| r.cost_microusd).sum(),
        rows,
        agreed,
        compared,
        outcome: outcome.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_parse_from_a_list_a_map_or_json_wrapped_in_text() {
        let list = r#"Sure! {"stages": [{"stage": "review", "model": "Haiku", "reason": "small diff"}]} done"#;
        assert_eq!(
            parse_picks(list),
            vec![ShadowPick {
                stage: "review".into(),
                model: "haiku".into(),
                reason: "small diff".into()
            }]
        );
        let map = r#"{"implement": "sonnet", "review": "haiku"}"#;
        assert_eq!(parse_picks(map).len(), 2);
        assert!(parse_picks("no json here").is_empty());
        assert!(parse_picks("{not json}").is_empty());
    }

    #[test]
    fn comparison_matches_model_families_per_stage() {
        let plan = vec![
            "implement".to_owned(),
            "review".to_owned(),
            "fix".to_owned(),
        ];
        let decision = Decision {
            decider: "ollama:qwen3.5:4b".into(),
            arm: "shadow".into(),
            why: String::new(),
            picks: parse_picks(r#"{"implement": "sonnet", "review": "haiku"}"#),
        };
        let ran = |stage: &str, model: &str, cost: u64| Ran {
            stage: stage.into(),
            model: model.into(),
            cost_microusd: Some(cost),
        };
        let actual = vec![
            ran("implement", "claude-sonnet-5-5", 2_000_000),
            ran("review", "claude-opus-5-5", 900_000),
        ];
        let r = compare(&decision, &plan, &actual, "completed");
        assert_eq!((r.agreed, r.compared), (1, 2));
        assert_eq!(r.cost_microusd, 2_900_000);
        assert_eq!(r.rows[1].cost_microusd, Some(900_000));
        assert!(r.rows[0].agrees);
        assert!(!r.rows[1].agrees);
        assert_eq!(r.rows[2].picked, None);
    }

    #[test]
    fn risky_jobs_stay_in_shadow_and_the_rest_split_by_a_stable_bucket() {
        let flags = vec!["production".to_owned()];
        assert_eq!(arm_for("J1", &flags, 100).0, "shadow");
        assert_eq!(arm_for("J1", &[], 100).0, "live");
        assert_eq!(arm_for("J1", &[], 0).0, "shadow");
        assert_eq!(bucket("01M78YVD"), bucket("01M78YVD"));
        let live = (0..1000)
            .filter(|i| arm_for(&format!("job-{i}"), &[], 50).0 == "live")
            .count();
        assert!((350..650).contains(&live), "{live}");
    }

    #[test]
    fn only_the_live_arm_runs_picks_and_never_opus() {
        let mut d = Decision {
            decider: "d".into(),
            arm: "live".into(),
            why: String::new(),
            picks: parse_picks(r#"{"grill": "haiku", "implement": "opus", "review": "Sonnet"}"#),
        };
        assert_eq!(d.live_model("grill"), Some("haiku"));
        assert_eq!(d.live_model("review"), Some("sonnet"));
        assert_eq!(d.live_model("implement"), None);
        assert_eq!(d.live_model("spec"), None);
        d.arm = "shadow".into();
        assert_eq!(d.live_model("grill"), None);
    }

    #[test]
    fn runs_keep_the_last_model_and_sum_cost_over_attempts() {
        use crate::event::Actor;
        use crate::id::JobId;
        use crate::usage::Usage;
        let ev = |seq: u64, payload: EventPayload| Event {
            id: format!("e{seq}"),
            job_id: JobId::new("J"),
            stage_id: Some("S1".into()),
            seq,
            ts_ms: 0,
            actor: Actor::Daemon,
            payload,
        };
        let finished = |seq, model: &str, cost| {
            ev(
                seq,
                EventPayload::AgentFinished {
                    usage: Usage {
                        model: Some(model.into()),
                        cost_microusd: Some(cost),
                        ..Default::default()
                    },
                },
            )
        };
        let events = vec![
            ev(
                1,
                EventPayload::StagePlanned {
                    n: 1,
                    name: "implement".into(),
                },
            ),
            finished(2, "haiku", 100),
            finished(3, "sonnet", 400),
        ];
        assert_eq!(
            actual_runs(&events),
            vec![Ran {
                stage: "implement".into(),
                model: "sonnet".into(),
                cost_microusd: Some(500)
            }]
        );
    }
}
