use pizza_delivery_core::model_shadow::{
    actual_runs, arm_for, compare, parse_picks, shadow_prompt, Decision, DEFAULT_SOP,
};

use super::*;

const DECISION_KIND: &str = "model-decider";
const SHADOW_KIND: &str = "model-shadow";

impl Worker<'_> {
    /// The decider's answer for a job, asked once and stored as an artifact
    /// so every stage (and the end-of-job record) sees the same picks. None
    /// when the trial is off or triage has not run. A failed call is stored
    /// too, as a shadow-arm decision with no picks, so it is not retried on
    /// every stage.
    pub(super) fn decision(&self, job: &JobId, events: &[Event]) -> Option<Decision> {
        let shadow = self.config.shadow.as_ref()?;
        let stored = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ArtifactRecorded { kind, path, .. } if kind == DECISION_KIND => {
                Some(path.clone())
            }
            _ => None,
        });
        if let Some(path) = stored {
            return std::fs::read(self.config.data_dir.join(path))
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
        }
        let triage = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::TriageCompleted { triage } => Some(triage),
            _ => None,
        })?;
        let decider = format!("ollama:{}", shadow.model);
        let (arm, why) = arm_for(job.as_str(), &triage.risk_flags, shadow.live_percent);
        let decision = match self.ask_decider(shadow, triage) {
            Ok(picks) => Decision {
                decider,
                arm: arm.to_owned(),
                why,
                picks,
            },
            Err(e) => Decision {
                decider,
                arm: "shadow".to_owned(),
                why: format!("decider gave no answer: {e:#}"),
                picks: vec![],
            },
        };
        let relative = PathBuf::from("jobs")
            .join(job.as_str())
            .join("model-decider.json");
        let written = serde_json::to_vec_pretty(&decision)
            .map_err(anyhow::Error::from)
            .and_then(|bytes| {
                let path = self.config.data_dir.join(&relative);
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(path, bytes)?;
                self.record_file(job, None, DECISION_KIND, &relative)
            });
        written.ok().map(|()| decision)
    }

    fn ask_decider(
        &self,
        shadow: &crate::config::ShadowConfig,
        triage: &pizza_delivery_core::triage::Triage,
    ) -> Result<Vec<pizza_delivery_core::model_shadow::ShadowPick>> {
        let sop = shadow
            .sop
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| DEFAULT_SOP.to_owned());
        let body = serde_json::json!({
            "model": shadow.model,
            "messages": [{"role": "user", "content": shadow_prompt(&sop, triage)}],
            "stream": false,
            "think": false,
            "format": "json",
            "options": {"temperature": 0},
        });
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(120))
            .build();
        let reply = agent
            .post(&format!("{}/api/chat", shadow.url.trim_end_matches('/')))
            .set("Content-Type", "application/json")
            .send_string(&body.to_string())?
            .into_string()?;
        let reply: serde_json::Value = serde_json::from_str(&reply)?;
        let picks = parse_picks(reply["message"]["content"].as_str().unwrap_or_default());
        if picks.is_empty() {
            return Err(anyhow!("no stage picks in the reply"));
        }
        Ok(picks)
    }

    /// Once a job has ended, records the decider's picks beside the models
    /// that actually ran, their cost and the outcome. Best effort.
    pub(super) fn shadow_if_due(&self, job: &JobId) {
        if self.config.shadow.is_none() {
            return;
        }
        let Ok(events) = self.with_store(|s| s.read_timeline(job)) else {
            return;
        };
        let ended = events.iter().find_map(|e| match &e.payload {
            EventPayload::JobCompleted {} => Some("completed".to_owned()),
            EventPayload::JobFailed { reason } => Some(format!("failed: {reason}")),
            EventPayload::JobNeedsHuman { reason } => Some(format!("needs human: {reason}")),
            _ => None,
        });
        let done = events.iter().any(|e| {
            matches!(&e.payload, EventPayload::ArtifactRecorded { kind, .. } if kind == SHADOW_KIND)
        });
        let Some(outcome) = ended.filter(|_| !done) else {
            return;
        };
        let _ = self.shadow(job, &events, &outcome);
    }

    fn shadow(&self, job: &JobId, events: &[Event], outcome: &str) -> Result<()> {
        let decision = self
            .decision(job, events)
            .ok_or_else(|| anyhow!("no decision"))?;
        let plan: Vec<String> = events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::TriageCompleted { triage } => {
                    Some(triage.plan.iter().map(|s| s.name.clone()).collect())
                }
                _ => None,
            })
            .unwrap_or_default();
        let record = compare(&decision, &plan, &actual_runs(events), outcome);
        let relative = PathBuf::from("jobs")
            .join(job.as_str())
            .join("model-shadow.json");
        let path = self.config.data_dir.join(&relative);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
        self.record_file(job, None, SHADOW_KIND, &relative)
    }
}
