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
    ) -> Result<std::result::Result<Delivery, String>> {
        let triaged = events
            .iter()
            .any(|e| matches!(e.payload, EventPayload::TriageCompleted { .. }));
        if !triaged {
            return Ok(Ok(Delivery::Done));
        }
        if current_plan(events).iter().any(|s| s.name == "ship") {
            self.ship(job, events)
        } else {
            self.report(job, events)?;
            Ok(Ok(Delivery::Done))
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
        readiness: Result<MergeReadiness>,
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
        match readiness {
            Ok(MergeReadiness::Ready) => {}
            Ok(MergeReadiness::ReadyWithoutChecks) if local_model_wrote_code(events) => {
                return Err(
                    "a local model wrote the code and the repository has no CI checks; \
                     left open for a human"
                        .to_owned(),
                )
            }
            Ok(MergeReadiness::ReadyWithoutChecks) => {}
            Ok(MergeReadiness::Blocked(reason)) => return Err(reason),
            Err(e) => return Err(format!("could not check merge readiness: {e:#}")),
        }
        self.git
            .merge_pr(worktree, url)
            .map_err(|e| format!("merge failed: {e:#}"))
    }

    /// A red CI gets one automatic fix round, unless the same checks already
    /// fail on the base branch or a round has been spent.
    fn red_ci(
        &self,
        job: &JobId,
        worktree: &Path,
        url: &str,
        reason: &str,
        events: &[Event],
    ) -> Result<Delivery> {
        let human = |why: &str| {
            Delivery::NeedsHuman(format!(
                "CI is red on {url} ({reason}{why}); the PR is open, fix it or the checks"
            ))
        };
        let spent = events.iter().any(|e| {
            matches!(&e.payload, EventPayload::ArtifactRecorded { kind, .. } if kind == "ci-failure")
        });
        if spent {
            return Ok(human("; one automatic fix round did not clear it"));
        }
        let failure = match self.git.ci_failure(worktree, url) {
            Ok(f) => f,
            Err(e) => return Ok(human(&format!("; could not read the failure: {e:#}"))),
        };
        if failure.base_also_failing {
            return Ok(human("; the base branch fails the same checks"));
        }
        let Some(last) = pizza_delivery_core::pipeline(events).pop() else {
            return Ok(human(""));
        };
        let mut added = vec![PlannedStage {
            name: "fix".to_owned(),
            reason: "fix the failing CI checks".to_owned(),
        }];
        // The fix is new code: it gets reviewed before it can merge.
        if current_plan(events).iter().any(|s| s.name == "review") {
            added.push(PlannedStage {
                name: "review".to_owned(),
                reason: "re-review after the CI fix".to_owned(),
            });
        }
        if amend_plan(
            &current_plan(events),
            &last,
            added.clone(),
            DEFAULT_MAX_STAGES,
        )
        .is_err()
        {
            return Ok(human("; the plan has no room for a fix round"));
        }
        let report = format!(
            "# CI failed\n\nFailing checks: {}\n\nThese checks failed on the pull request. Reproduce them locally, fix the cause and commit. A failure that also exists on the base branch is not yours to fix.\n\n## Log tail\n\n```\n{}\n```\n",
            failure.checks.join(", "),
            failure.log
        );
        let relative = PathBuf::from("jobs")
            .join(job.as_str())
            .join("ci-failure.md");
        let path = self.config.data_dir.join(&relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, report)?;
        self.record_file(job, None, "ci-failure", &relative)?;
        self.append(
            job,
            None,
            EventPayload::PlanAmended {
                after: last,
                stages: added,
            },
        )?;
        Ok(Delivery::Retry)
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
    ) -> Result<std::result::Result<Delivery, String>> {
        let Some(found) = self.find_job(job)? else {
            return Ok(Err("ship: job vanished".to_owned()));
        };
        let Some(repo) = found.repo.as_deref().map(PathBuf::from) else {
            return Ok(Err("ship: job has no repository".to_owned()));
        };
        let worktree = self.worktree_of(&repo, job);
        let (target, on_pr) = self.work_branch(&repo, job, &found.prompt);
        // On a PR the local branch is the job's own; it pushes to the PR's.
        let branch = if on_pr {
            format!("pizza/{job}:{target}")
        } else {
            target
        };
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
        if files.is_empty() {
            // Nothing to ship (report-only or investigation work): pushing an
            // empty branch makes `gh pr create` fail with "No commits between".
            self.report(job, events)?;
            return Ok(Ok(Delivery::Done));
        }
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
                // Always wait for every automatic check, whether or not the
                // PR is eligible to merge: a red CI must not look like a
                // finished job.
                let readiness = self.git.merge_readiness(&worktree, &url);
                let failing = match &readiness {
                    Ok(MergeReadiness::Blocked(reason)) if reason.starts_with("checks failing") => {
                        Some(reason.clone())
                    }
                    _ => None,
                };
                let outcome = self.merge_outcome(events, &policy, &worktree, &url, readiness);
                self.append(
                    job,
                    None,
                    match outcome {
                        Ok(()) => EventPayload::PrMerged {
                            pr_url: url.clone(),
                        },
                        Err(reason) => EventPayload::PrNotMerged {
                            pr_url: Some(url.clone()),
                            reason,
                        },
                    },
                )?;
                Ok(Ok(match failing {
                    Some(reason) => self.red_ci(job, &worktree, &url, &reason, events)?,
                    None => Delivery::Done,
                }))
            }
            Err(e) => {
                audit(Some(pushed), None)?;
                Ok(Err(format!("ship: opening the pull request failed: {e:#}")))
            }
        }
    }
}

/// How a job's delivery ended.
pub(super) enum Delivery {
    Done,
    /// Shipped, but a human has to act (for example a red CI).
    NeedsHuman(String),
    /// The plan was amended with a fix round; deliver again afterwards.
    Retry,
}

/// Whether an implement or fix stage ran on a local (Ollama) model, whose
/// work should not merge on review alone.
fn local_model_wrote_code(events: &[Event]) -> bool {
    let coding: Vec<String> = pizza_delivery_core::fold_stages(events)
        .into_iter()
        .filter(|s| s.name == "implement" || s.name == "fix")
        .map(|s| s.id)
        .collect();
    events.iter().any(|e| match &e.payload {
        EventPayload::AgentFinished { usage } => {
            e.stage_id.as_ref().is_some_and(|id| coding.contains(id))
                && usage
                    .model
                    .as_deref()
                    .is_some_and(|m| m.starts_with("ollama/"))
        }
        _ => false,
    })
}
