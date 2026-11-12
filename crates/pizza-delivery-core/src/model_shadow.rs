//! Shadow pilot for an LLM model decider. A local model (for example Qwen on
//! Ollama) is asked which model each stage should have used, given only what
//! triage knew. Its answer is recorded beside the models that actually ran,
//! and never changes routing. The comparison is what decides whether the LLM
//! rung is worth promoting.

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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowRecord {
    pub decider: String,
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

/// The models each stage actually ran on, from the ledger.
pub fn actual_models(events: &[Event]) -> Vec<(String, String)> {
    let stage_of = |id: &str| {
        events.iter().find_map(|e| match &e.payload {
            EventPayload::StagePlanned { name, .. } if e.stage_id.as_deref() == Some(id) => {
                Some(name.clone())
            }
            _ => None,
        })
    };
    let mut out: Vec<(String, String)> = vec![];
    for e in events {
        let EventPayload::AgentFinished { usage } = &e.payload else {
            continue;
        };
        let stage = e.stage_id.as_deref().and_then(stage_of);
        if let (Some(stage), Some(model)) = (stage, &usage.model) {
            out.retain(|(s, _)| *s != stage);
            out.push((stage, model.clone()));
        }
    }
    out
}

pub fn compare(
    decider: &str,
    plan: &[String],
    picks: &[ShadowPick],
    actual: &[(String, String)],
    outcome: &str,
) -> ShadowRecord {
    let rows: Vec<ShadowRow> = plan
        .iter()
        .map(|stage| {
            let pick = picks.iter().find(|p| &p.stage == stage);
            let ran = actual.iter().find(|(s, _)| s == stage).map(|(_, m)| m.clone());
            let agrees = match (pick, &ran) {
                (Some(p), Some(r)) => family(&p.model) == family(r),
                _ => false,
            };
            ShadowRow {
                stage: stage.clone(),
                picked: pick.map(|p| p.model.clone()),
                reason: pick.map(|p| p.reason.clone()).filter(|r| !r.is_empty()),
                actual: ran,
                agrees,
            }
        })
        .collect();
    let compared = rows
        .iter()
        .filter(|r| r.picked.is_some() && r.actual.is_some())
        .count();
    let agreed = rows.iter().filter(|r| r.agrees).count();
    ShadowRecord {
        decider: decider.to_owned(),
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
        let plan = vec!["implement".to_owned(), "review".to_owned(), "fix".to_owned()];
        let picks = parse_picks(r#"{"implement": "sonnet", "review": "haiku"}"#);
        let actual = vec![
            ("implement".to_owned(), "claude-sonnet-5-5".to_owned()),
            ("review".to_owned(), "claude-opus-5-5".to_owned()),
        ];
        let r = compare("ollama:qwen3.5:4b", &plan, &picks, &actual, "completed");
        assert_eq!((r.agreed, r.compared), (1, 2));
        assert!(r.rows[0].agrees);
        assert!(!r.rows[1].agrees);
        assert_eq!(r.rows[2].picked, None);
    }
}
