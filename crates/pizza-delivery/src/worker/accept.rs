use super::*;

impl Worker<'_> {
    pub(super) fn record_feedback(
        &self,
        claimed: &Claimed,
        actor: Actor,
        entries: Vec<Feedback>,
    ) -> Result<()> {
        for f in entries {
            let event = NewEvent {
                id: self.env.new_ulid(),
                job_id: claimed.job.id.clone(),
                stage_id: Some(claimed.stage_id.clone()),
                ts_ms: self.env.now_ms(),
                actor,
                payload: EventPayload::FeedbackRecorded {
                    kind: f.kind,
                    severity: f.severity,
                    text: f.text,
                    confidence: f.confidence,
                    alternatives: f.alternatives,
                    impact: f.impact,
                    evidence: f.evidence,
                },
            };
            self.with_store(|s| s.append_event(event))?;
        }
        Ok(())
    }

    /// Agent-written items pass the confidence gate: only those it sends to the
    /// inbox become feedback; the rest are kept as suppressed audit events.
    fn record_gated_feedback(&self, claimed: &Claimed, entries: Vec<Feedback>) -> Result<()> {
        let (inbox, rest): (Vec<_>, Vec<_>) = entries
            .into_iter()
            .partition(|f| self.gate_decision(f) == GateDecision::Inbox);
        self.record_feedback(claimed, Actor::Agent, inbox)?;
        for f in rest {
            let decision = self.gate_decision(&f);
            self.append(
                &claimed.job.id,
                Some(&claimed.stage_id),
                EventPayload::FeedbackSuppressed {
                    kind: f.kind,
                    text: f.text,
                    confidence: f.confidence,
                    impact: f.impact.as_deref().and_then(Impact::parse),
                    decision,
                },
            )?;
        }
        Ok(())
    }

    fn gate_decision(&self, f: &Feedback) -> GateDecision {
        gate(
            GateItem {
                confidence: f.confidence,
                impact: f.impact.as_deref().and_then(Impact::parse),
            },
            self.config.gate,
        )
    }

    pub(super) fn accept_output(&self, claimed: &Claimed, run: &StageRun) -> Result<Verdict> {
        let handoff_rel = run.handoff_rel.as_path();
        let path = self.config.data_dir.join(handoff_rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                "agent wrote no handoff".to_owned(),
            ));
        };
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "handoff", handoff_rel)?;
        if let Err(e) = validate(&text) {
            return Ok(Verdict::Retry(
                RetryCause::InvalidHandoff,
                format!("invalid handoff: {e}"),
            ));
        }
        let outcome = match claimed.name.as_str() {
            "grill" => self.accept_grill(claimed, run)?,
            "spec" => self.accept_spec(claimed, run)?,
            "review" => self.accept_review(claimed, run)?,
            _ => Ok(Vec::new()),
        };
        let extra = match outcome {
            Ok(extra) => extra,
            Err(reason) => return Ok(Verdict::Retry(RetryCause::InvalidHandoff, reason)),
        };
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::HandoffWritten {
                path: handoff_rel.to_string_lossy().into_owned(),
            },
        )?;
        self.record_gated_feedback(claimed, parse_feedback(&text))?;
        self.record_feedback(claimed, Actor::Daemon, extra)?;
        if claimed.name == "review" {
            self.plan_review_follow_up(claimed, run)?;
        }
        Ok(Verdict::Succeeded)
    }

    pub(super) fn accept_grill(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let rel = run.relative.join("grill.md");
        let Some(transcript) = self.read_data(&rel.to_string_lossy()) else {
            return Ok(Ok(Vec::new()));
        };
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "grill-transcript", &rel)?;
        Ok(Ok(
            grill::assess(&transcript, grill::DEFAULT_ROUND_CAP).feedback
        ))
    }

    pub(super) fn accept_spec(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let dir = run.worktree.join(spec_dir(&claimed.job.id));
        let Ok(spec_text) = std::fs::read_to_string(dir.join("spec.md")) else {
            return Ok(Err("invalid spec: agent wrote no spec.md".to_owned()));
        };
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("tickets"))
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "md"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let tickets: Vec<(String, String)> = files
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some((name, std::fs::read_to_string(p).ok()?))
            })
            .collect();
        if let Err(e) = spec::validate(&spec_text, &tickets) {
            return Ok(Err(format!("invalid spec: {e}")));
        }
        let worktree_rel = run
            .worktree
            .strip_prefix(&self.config.data_dir)
            .unwrap_or(&run.worktree)
            .join(spec_dir(&claimed.job.id));
        self.record_artifact(
            &claimed.job.id,
            &claimed.stage_id,
            "spec",
            &worktree_rel.join("spec.md"),
        )?;
        for file in &files {
            if let Some(name) = file.file_name() {
                self.record_artifact(
                    &claimed.job.id,
                    &claimed.stage_id,
                    "ticket",
                    &worktree_rel.join("tickets").join(name),
                )?;
            }
        }
        Ok(Ok(Vec::new()))
    }

    pub(super) fn accept_review(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Result<Vec<Feedback>, String>> {
        let rel = run.relative.join("review.md");
        let Some(report) = self.read_data(&rel.to_string_lossy()) else {
            return Ok(Err("invalid review: agent wrote no review.md".to_owned()));
        };
        if let Err(e) = review::parse_report(&report) {
            return Ok(Err(format!("invalid review: {e}")));
        }
        self.record_artifact(&claimed.job.id, &claimed.stage_id, "review-report", &rel)?;
        Ok(Ok(Vec::new()))
    }

    pub(super) fn plan_review_follow_up(&self, claimed: &Claimed, run: &StageRun) -> Result<()> {
        let report = self
            .read_data(&run.relative.join("review.md").to_string_lossy())
            .unwrap_or_default();
        let findings = review::parse_report(&report).unwrap_or_default();
        let events = self.with_store(|s| s.read_timeline(&claimed.job.id))?;
        let added = vec![
            PlannedStage {
                name: "fix".to_owned(),
                reason: "fix the blocking review findings".to_owned(),
            },
            PlannedStage {
                name: "review".to_owned(),
                reason: "re-review after the fix".to_owned(),
            },
        ];
        let can_plan = amend_plan(
            &current_plan(&events),
            "review",
            added.clone(),
            DEFAULT_MAX_STAGES,
        )
        .is_ok();
        match review::decide(&findings, fix_rounds(&events), can_plan) {
            Decision::Clean => Ok(()),
            Decision::Fix => self.append(
                &claimed.job.id,
                None,
                EventPayload::PlanAmended {
                    after: "review".to_owned(),
                    stages: added,
                },
            ),
            Decision::Leftover(entries) => self.record_feedback(claimed, Actor::Daemon, entries),
        }
    }
}
