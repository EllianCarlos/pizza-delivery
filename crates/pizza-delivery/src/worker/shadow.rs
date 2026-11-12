use pizza_delivery_core::model_shadow::{
    actual_models, compare, parse_picks, shadow_prompt, DEFAULT_SOP,
};

use super::*;

const SHADOW_KIND: &str = "model-shadow";

impl Worker<'_> {
    /// Once a job has ended, asks the shadow decider which models it would
    /// have used and records that beside what actually ran. Best effort: any
    /// failure is ignored, and routing never reads the result.
    pub(super) fn shadow_if_due(&self, job: &JobId) {
        let Some(shadow) = &self.config.shadow else {
            return;
        };
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
        let _ = self.shadow(job, &events, shadow, &outcome);
    }

    fn shadow(
        &self,
        job: &JobId,
        events: &[Event],
        shadow: &crate::config::ShadowConfig,
        outcome: &str,
    ) -> Result<()> {
        let triage = events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::TriageCompleted { triage } => Some(triage),
                _ => None,
            })
            .ok_or_else(|| anyhow!("no triage"))?;
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
        let text = reply["message"]["content"].as_str().unwrap_or_default();
        let plan: Vec<String> = triage.plan.iter().map(|s| s.name.clone()).collect();
        let record = compare(
            &format!("ollama:{}", shadow.model),
            &plan,
            &parse_picks(text),
            &actual_models(events),
            outcome,
        );
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
