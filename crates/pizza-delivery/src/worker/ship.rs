use super::*;

impl Worker<'_> {
    pub(super) fn record_artifact(
        &self,
        job: &JobId,
        stage_id: &str,
        kind: &str,
        relative: &Path,
    ) -> Result<()> {
        self.record_file(job, Some(stage_id), kind, relative)
    }

    pub(super) fn record_file(
        &self,
        job: &JobId,
        stage_id: Option<&str>,
        kind: &str,
        relative: &Path,
    ) -> Result<()> {
        let bytes = std::fs::read(self.config.data_dir.join(relative))?;
        self.append(
            job,
            stage_id,
            EventPayload::ArtifactRecorded {
                kind: kind.to_owned(),
                path: relative.to_string_lossy().into_owned(),
                sha256: sha256_hex(&bytes),
            },
        )
    }

    pub(super) fn handoffs(&self, events: &[Event]) -> Vec<(String, String)> {
        let stages = fold_stages(events);
        let mut latest: std::collections::BTreeMap<u32, (String, PathBuf)> =
            std::collections::BTreeMap::new();
        for event in events {
            if let (EventPayload::HandoffWritten { path }, Some(id)) =
                (&event.payload, &event.stage_id)
            {
                if let Some(stage) = stages.iter().find(|s| &s.id == id) {
                    latest.insert(stage.n, (stage.name.clone(), PathBuf::from(path)));
                }
            }
        }
        latest
            .into_values()
            .filter_map(|(name, path)| {
                std::fs::read_to_string(self.config.data_dir.join(path))
                    .ok()
                    .map(|text| (name, text))
            })
            .collect()
    }

    pub(super) fn deliver(
        &self,
        job: &JobId,
        events: &[Event],
    ) -> Result<std::result::Result<(), String>> {
        let triaged = events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::TriageCompleted { .. }));
        if !triaged {
            return Ok(Ok(()));
        }
        if current_plan(events).iter().any(|s| s.name == "ship") {
            self.ship(job, events)
        } else {
            self.report(job, events)?;
            Ok(Ok(()))
        }
    }

    pub(super) fn report(&self, job: &JobId, events: &[Event]) -> Result<()> {
        let Some(found) = self.find_job(job)? else {
            return Ok(());
        };
        let body = render_pr_body(
            job.as_str(),
            &found.prompt,
            &self.handoffs(events),
            &DiffPolicy::evaluate(&[]),
        );
        let relative = PathBuf::from("jobs").join(job.as_str()).join("report.md");
        let path = self.config.data_dir.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body)?;
        self.record_file(job, None, "report", &relative)
    }

    /// Merges the shipped PR when every condition holds; otherwise returns
    /// the reason it was left open. Never fails the job.
    fn merge_outcome(
        &self,
        events: &[Event],
        policy: &DiffPolicy,
        worktree: &Path,
        url: &str,
    ) -> std::result::Result<(), String> {
        if !self.config.auto_merge {
            return Err("auto-merge is disabled".to_owned());
        }
        if policy.is_sensitive() {
            return Err(format!(
                "diff policy: infrastructure files changed ({}); left as a labelled draft",
                policy.sensitive.join(", ")
            ));
        }
        self.review_passed(events)?;
        self.parent_merged(events)?;
        match self.git.merge_readiness(worktree, url) {
            Ok(MergeReadiness::Ready) => {}
            Ok(MergeReadiness::Blocked(reason)) => return Err(reason),
            Err(e) => return Err(format!("could not check merge readiness: {e:#}")),
        }
        self.git
            .merge_pr(worktree, url)
            .map_err(|e| format!("merge failed: {e:#}"))
    }

    fn review_passed(&self, events: &[Event]) -> std::result::Result<(), String> {
        if !current_plan(events).iter().any(|s| s.name == "review") {
            return Err("the plan has no review stage".to_owned());
        }
        let report = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ArtifactRecorded { kind, path, .. } if kind == "review-report" => {
                Some(path.clone())
            }
            _ => None,
        });
        let Some(text) = report.and_then(|path| self.read_data(&path)) else {
            return Err("no review report was recorded".to_owned());
        };
        match review::parse_report(&text) {
            Ok(findings) if review::blocking(&findings).is_empty() => Ok(()),
            Ok(_) => Err("the final review still has blocking findings".to_owned()),
            Err(e) => Err(format!("the review report is invalid: {e}")),
        }
    }

    /// Stacked PRs merge in stack order: a follow-up job's PR carries its
    /// parent's commits, so the parent's PR must be merged first.
    fn parent_merged(&self, events: &[Event]) -> std::result::Result<(), String> {
        let parent = events.iter().find_map(|e| match &e.payload {
            EventPayload::JobSubmitted {
                follow_up: Some(f), ..
            } => Some(f.parent.clone()),
            _ => None,
        });
        let Some(parent) = parent else {
            return Ok(());
        };
        let parent_events = self
            .with_store(|s| s.read_timeline(&parent))
            .map_err(|e| format!("could not read parent job {parent}: {e:#}"))?;
        let has_pr = parent_events.iter().any(|e| {
            matches!(
                e.payload,
                EventPayload::SandboxAudit {
                    pr_url: Some(_),
                    ..
                }
            )
        });
        let merged = parent_events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::PrMerged { .. }));
        if has_pr && !merged {
            return Err(format!(
                "the parent job {parent}'s PR is not merged yet; stacked PRs merge parent first"
            ));
        }
        Ok(())
    }

    pub(super) fn ship(
        &self,
        job: &JobId,
        events: &[Event],
    ) -> Result<std::result::Result<(), String>> {
        let Some(found) = self.find_job(job)? else {
            return Ok(Err("ship: job vanished".to_owned()));
        };
        let Some(repo) = found.repo.as_deref().map(PathBuf::from) else {
            return Ok(Err("ship: job has no repository".to_owned()));
        };
        let worktree = self.worktree_of(&repo, job);
        let branch = format!("pizza/{job}");
        let committed = match self.git.is_dirty(&worktree) {
            Ok(true) => self
                .git
                .commit_wip(&worktree, "pizza-delivery: uncommitted changes")
                .map(|_| ()),
            Ok(false) => Ok(()),
            Err(e) => Err(e),
        };
        let files = match committed.and_then(|()| self.git.changed_files(&repo, &worktree)) {
            Ok(files) => files,
            Err(e) => return Ok(Err(format!("ship: {e:#}"))),
        };
        let policy = DiffPolicy::evaluate(&files);
        if let Some(risk) = policy.risk() {
            let event = NewEvent {
                id: self.env.new_ulid(),
                job_id: job.clone(),
                stage_id: None,
                ts_ms: self.env.now_ms(),
                actor: Actor::Daemon,
                payload: EventPayload::FeedbackRecorded {
                    kind: risk.kind,
                    severity: risk.severity,
                    text: risk.text,
                    confidence: risk.confidence,
                    alternatives: risk.alternatives,
                    impact: risk.impact,
                    evidence: risk.evidence,
                },
            };
            self.with_store(|s| s.append_event(event))?;
        }
        let remote = self
            .config
            .trust
            .repos
            .get(repo.to_string_lossy().as_ref())
            .and_then(|t| t.push_remote.clone())
            .unwrap_or_else(|| "origin".to_owned());
        let audit = |pushed_ref: Option<String>, pr_url: Option<String>| {
            self.append(
                job,
                None,
                EventPayload::SandboxAudit {
                    profile_hash: "daemon-ship".to_owned(),
                    mounts: vec![],
                    egress_allowlist: vec![],
                    blocked_requests: vec![],
                    mcp_activations: vec![],
                    diff_policy_result: Some(policy.result()),
                    pushed_ref,
                    pr_url,
                },
            )
        };
        let pushed = match self.git.push(&worktree, &remote, &branch) {
            Ok(pushed) => pushed,
            Err(e) => {
                audit(None, None)?;
                return Ok(Err(format!("ship: push failed: {e:#}")));
            }
        };
        let body = render_pr_body(job.as_str(), &found.prompt, &self.handoffs(events), &policy);
        let relative = PathBuf::from("jobs").join(job.as_str()).join("pr-body.md");
        let path = self.config.data_dir.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &body)?;
        self.record_file(job, None, "pr-body", &relative)?;
        match self.git.open_draft_pr(
            &worktree,
            &pr_title(job.as_str(), &found.prompt),
            &body,
            &policy.labels(),
        ) {
            Ok(url) => {
                audit(Some(pushed), Some(url.clone()))?;
                let outcome = self.merge_outcome(events, &policy, &worktree, &url);
                self.append(
                    job,
                    None,
                    match outcome {
                        Ok(()) => EventPayload::PrMerged { pr_url: url },
                        Err(reason) => EventPayload::PrNotMerged {
                            pr_url: Some(url),
                            reason,
                        },
                    },
                )?;
                Ok(Ok(()))
            }
            Err(e) => {
                audit(Some(pushed), None)?;
                Ok(Err(format!("ship: opening the pull request failed: {e:#}")))
            }
        }
    }
}
