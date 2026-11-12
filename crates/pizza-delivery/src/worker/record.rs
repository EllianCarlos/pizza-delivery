use super::*;

impl Worker<'_> {
    pub(super) fn record_budget(
        &self,
        claimed: &Claimed,
        agent: &str,
        actual_tokens: Option<u64>,
        actual_wall_ms: Option<u64>,
    ) -> Result<Option<String>> {
        let job = &claimed.job.id;
        let stage = Some(claimed.stage_id.as_str());
        let events = self.with_store(|s| s.read_timeline(job))?;
        let estimate = estimate_for(&events, &claimed.name).unwrap_or_else(|| {
            estimate_stage(
                &claimed.name,
                tier_for(&claimed.name),
                agent,
                &[],
                &self.config.budget_defaults,
            )
        });
        self.append(
            job,
            stage,
            EventPayload::StageBudgetRecorded {
                stage: claimed.name.clone(),
                tier: estimate.tier,
                agent: agent.to_owned(),
                estimate_tokens: estimate.tokens,
                estimate_wall_ms: estimate.wall_ms,
                actual_tokens,
                actual_wall_ms,
            },
        )?;
        let events = self.with_store(|s| s.read_timeline(job))?;
        let Some(f) = actual_overrun(&events, self.config.budget_cap_tokens) else {
            return Ok(None);
        };
        self.append(
            job,
            None,
            EventPayload::FeedbackRecorded {
                kind: f.kind,
                severity: f.severity,
                text: f.text.clone(),
                confidence: f.confidence,
                alternatives: f.alternatives,
                impact: f.impact,
                evidence: f.evidence,
            },
        )?;
        Ok(Some(f.text))
    }

    pub(super) fn record_mcp(&self, claimed: &Claimed, log: &Path) -> Result<Vec<String>> {
        let mut activated = Vec::new();
        for event in crate::mcp_broker::read_events(log) {
            if let McpEvent::Activated { server, .. } = &event {
                activated.push(server.clone());
            }
            self.append(
                &claimed.job.id,
                Some(claimed.stage_id.as_str()),
                event.payload(),
            )?;
            if let Some(f) = event.feedback() {
                self.record_feedback(claimed, Actor::Agent, vec![f.clone()])?;
            }
        }
        Ok(activated)
    }

    pub(super) fn audit(
        &self,
        claimed: &Claimed,
        profile: &SandboxProfile,
        blocked_requests: Vec<String>,
        mcp_activations: Vec<String>,
    ) -> Result<()> {
        self.append(
            &claimed.job.id,
            Some(claimed.stage_id.as_str()),
            EventPayload::SandboxAudit {
                profile_hash: profile.hash(),
                mounts: profile.mounts.iter().map(Mount::describe).collect(),
                egress_allowlist: profile.egress_allowlist.clone(),
                blocked_requests,
                mcp_activations,
                diff_policy_result: None,
                pushed_ref: None,
                pr_url: None,
            },
        )
    }
}
