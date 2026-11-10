use super::*;

impl Worker<'_> {
    pub(super) fn beat(&self, stage_id: &str, stop: &AtomicBool) {
        let interval = Duration::from_millis(self.config.heartbeat_ms);
        let mut waited = Duration::ZERO;
        while !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
            waited += Duration::from_millis(10);
            if waited >= interval {
                waited = Duration::ZERO;
                let expires = self.env.now_ms() + self.config.lease_ttl_ms;
                let _ = self.with_store(|s| s.heartbeat(stage_id, expires));
            }
        }
    }

    pub(super) fn execute(&self, claimed: &Claimed) -> Result<Verdict> {
        let job = &claimed.job;
        let Some(repo) = job.repo.as_deref().map(PathBuf::from) else {
            return Ok(Verdict::Fail("job has no repository".to_owned()));
        };
        let worktree = self.worktree_of(&repo, &job.id);
        let base = self.parent_branch(&job.id)?;
        if let Err(e) = self.git.ensure_worktree(
            &repo,
            &worktree,
            &format!("pizza/{}", job.id),
            base.as_deref(),
        ) {
            return Ok(Verdict::Fail(format!("worktree: {e:#}")));
        }
        let relative = stage_dir(&job.id, claimed.n, &claimed.name);
        let dir = self.config.data_dir.join(&relative);
        std::fs::create_dir_all(&dir)?;
        let handoff_rel = relative.join("handoff.md");
        let stage = Some(claimed.stage_id.as_str());
        self.append(
            &job.id,
            stage,
            EventPayload::StageStarted {
                attempt: claimed.attempt,
            },
        )?;
        let run = StageRun {
            repo: repo.clone(),
            relative,
            handoff_path: self.config.data_dir.join(&handoff_rel),
            handoff_rel,
            worktree,
        };
        let quota = fold_quota(&self.with_store(|s| s.quota_events())?);
        let required = stage_capabilities(&claimed.name);
        let now = self.env.now_ms();
        let routed = match route(
            self.routing,
            &RouteRequest {
                stage: &claimed.name,
                tier: DEFAULT_TIER,
                repo: job.repo.as_deref(),
                required: &required,
            },
            &quota,
            now,
        ) {
            Ok(routed) => routed,
            Err(e) => return Ok(Verdict::Fail(format!("routing: {e}"))),
        };
        let mut waits: Vec<i64> = routed
            .skipped
            .iter()
            .map(|(_, skip)| match skip {
                Skip::Exhausted { until_ms } => *until_ms,
                Skip::Utilization { resets_at_ms } => {
                    resets_at_ms.unwrap_or(now + self.routing.default_backoff_ms)
                }
            })
            .collect();
        let mut note = stored_note(&dir, claimed.attempt);
        let mut last_error: Option<String> = None;
        for (position, candidate) in routed.candidates.iter().enumerate() {
            match self.try_candidate(claimed, &run, position, candidate, note.as_deref())? {
                Attempt::Settled(verdict) => return Ok(verdict),
                Attempt::Quota { until_ms, reason } => {
                    waits.push(until_ms);
                    self.fall_back(claimed, candidate, &reason)?;
                    note =
                        Some(self.interrupt(&Target::of(claimed), RetryCause::Quota, &reason)?);
                }
                Attempt::Provider(reason) => {
                    self.fall_back(claimed, candidate, &reason)?;
                    note = Some(format!(
                        "The previous agent stopped before finishing: {reason}"
                    ));
                    last_error = Some(reason);
                }
            }
        }
        Ok(match (waits.iter().min(), last_error) {
            (Some(until), _) => Verdict::Park {
                until_ms: (*until).max(self.env.now_ms() + MIN_PARK_MS),
                reason: "every candidate is out of quota".to_owned(),
            },
            (None, Some(reason)) => Verdict::Fail(reason),
            (None, None) => Verdict::Fail("no candidate could run".to_owned()),
        })
    }

    pub(super) fn fall_back(
        &self,
        claimed: &Claimed,
        candidate: &Candidate,
        reason: &str,
    ) -> Result<()> {
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::AgentFellBack {
                agent: candidate.agent.clone(),
                reason: reason.to_owned(),
            },
        )
    }

    pub(super) fn record_quota(
        &self,
        claimed: &Claimed,
        candidate: &Candidate,
        classified: &Classified,
    ) -> Result<Option<i64>> {
        let now = self.env.now_ms();
        let exhausted_by_failure = match &classified.failure {
            Some(Failure::QuotaExhausted { resets_at_ms, .. }) => Some(*resets_at_ms),
            _ => None,
        };
        let obs = classified.observation.as_ref();
        let signal = match (obs, exhausted_by_failure) {
            (Some(o), _) if o.status == QuotaStatus::Exhausted => {
                Some((o.status, o.utilization_permille, o.resets_at_ms))
            }
            (o, Some(reset)) => Some((
                QuotaStatus::Exhausted,
                o.and_then(|o| o.utilization_permille),
                reset.or_else(|| o.and_then(|o| o.resets_at_ms)),
            )),
            (Some(o), None) => Some((o.status, o.utilization_permille, o.resets_at_ms)),
            (None, None) => None,
        };
        let Some((status, utilization_permille, reset)) = signal else {
            return Ok(None);
        };
        let resets_at_ms = match (status, reset) {
            (QuotaStatus::Exhausted, None) => Some(now + self.routing.default_backoff_ms),
            (_, known) => known,
        };
        self.append(
            &claimed.job.id,
            Some(&claimed.stage_id),
            EventPayload::QuotaSignal {
                agent: candidate.agent.clone(),
                model: candidate.model.clone(),
                status,
                utilization_permille,
                resets_at_ms,
                reset_known: reset.is_some(),
            },
        )?;
        Ok((status == QuotaStatus::Exhausted)
            .then_some(resets_at_ms)
            .flatten())
    }

    pub(super) fn try_candidate(
        &self,
        claimed: &Claimed,
        run: &StageRun,
        position: usize,
        candidate: &Candidate,
        note: Option<&str>,
    ) -> Result<Attempt> {
        let job = &claimed.job;
        let stage = Some(claimed.stage_id.as_str());
        let Some((kind, agent)) = self.fleet.get(&candidate.agent) else {
            return Ok(Attempt::Settled(Verdict::Fail(format!(
                "agent '{}' is not configured",
                candidate.agent
            ))));
        };
        if run.handoff_path.exists() {
            std::fs::remove_file(&run.handoff_path)?;
        }
        let Some(rendered) = self.render_prompt(claimed, run)? else {
            return Ok(Attempt::Settled(Verdict::Fail(format!(
                "no prompt template for stage '{}'",
                claimed.name
            ))));
        };
        let rendered = rendered.with_interruption_note(note);
        self.append(
            &job.id,
            stage,
            EventPayload::AgentInvoked {
                agent: candidate.agent.clone(),
                template: rendered.template.clone(),
                version: rendered.version,
                prompt_sha256: rendered.prompt_sha256.clone(),
            },
        )?;
        let egress_rel = run.relative.join("egress-requests.log");
        let egress_log = self.config.data_dir.join(&egress_rel);
        if egress_log.exists() {
            std::fs::remove_file(&egress_log)?;
        }
        let sandbox = self.config.trust.profile_for(
            &run.repo.to_string_lossy(),
            &[
                run.worktree.to_string_lossy().into_owned(),
                self.config
                    .data_dir
                    .join(&run.relative)
                    .to_string_lossy()
                    .into_owned(),
            ],
        );
        let mcp_log = self
            .config
            .data_dir
            .join(run.relative.join("mcp-events.jsonl"));
        if mcp_log.exists() {
            std::fs::remove_file(&mcp_log)?;
        }
        let mcp_config = self
            .config
            .mcp
            .as_ref()
            .map(|setup| {
                let path = self.config.data_dir.join(run.relative.join("mcp.json"));
                let args = vec![
                    "mcp-broker".to_owned(),
                    "--catalog".to_owned(),
                    setup.catalog.to_string_lossy().into_owned(),
                    "--stage".to_owned(),
                    claimed.name.clone(),
                    "--agent".to_owned(),
                    candidate.agent.clone(),
                    "--log".to_owned(),
                    mcp_log.to_string_lossy().into_owned(),
                    "--secrets".to_owned(),
                    setup.secrets.to_string_lossy().into_owned(),
                ];
                let config = serde_json::json!({"mcpServers": {"pizza-delivery": {
                    "command": setup.broker,
                    "args": args,
                }}});
                std::fs::write(&path, config.to_string()).map(|()| path)
            })
            .transpose()?;
        let invocation = Invocation {
            mcp_config,
            prompt: rendered.text,
            model: candidate.model.clone(),
            cwd: run.worktree.clone(),
            handoff_path: run.handoff_path.clone(),
            timeout: self
                .config
                .timeout
                .unwrap_or_else(|| stage_timeout(&claimed.name, tier_of(&claimed.name))),
            grace: self.config.kill_grace,
            sandbox: sandbox.clone(),
            egress_log: egress_log.clone(),
        };
        let started = self.env.now_ms();
        let stop = AtomicBool::new(false);
        let output = std::thread::scope(|scope| {
            let beat = scope.spawn(|| self.beat(&claimed.stage_id, &stop));
            let result = agent.run(&invocation, &mut |pid| {
                let start = start_ticks(pid).unwrap_or_default();
                let _ = self.with_store(|s| s.set_lease_process(&claimed.stage_id, pid, start));
                if self.cancelled(&job.id) {
                    terminate_group(pid);
                }
            });
            stop.store(true, Ordering::SeqCst);
            let _ = beat.join();
            result
        });
        let requested = std::fs::read_to_string(&egress_log).unwrap_or_default();
        let blocked = blocked_hosts(&sandbox.egress_allowlist, &requested);
        for host in &blocked {
            self.append(
                &job.id,
                stage,
                EventPayload::EgressBlocked { host: host.clone() },
            )?;
        }
        let activations = self.record_mcp(claimed, &mcp_log)?;
        let output = match output {
            Ok(output) => {
                self.audit(claimed, &sandbox, blocked, activations)?;
                output
            }
            Err(e) => {
                self.audit(claimed, &sandbox, blocked, activations)?;
                return Ok(Attempt::Settled(Verdict::Fail(format!("agent: {e:#}"))));
            }
        };
        let wall_ms = u64::try_from(self.env.now_ms() - started).ok();
        let suffix = if position == 0 {
            String::new()
        } else {
            format!("-{position}")
        };
        let raw_rel = run.relative.join(format!("raw-output{suffix}.jsonl"));
        std::fs::write(self.config.data_dir.join(&raw_rel), &output.stdout)?;
        self.record_artifact(&job.id, &claimed.stage_id, "raw-output", &raw_rel)?;
        if !output.stderr.is_empty() {
            let err_rel = run.relative.join(format!("raw-stderr{suffix}.txt"));
            std::fs::write(self.config.data_dir.join(&err_rel), &output.stderr)?;
            self.record_artifact(&job.id, &claimed.stage_id, "raw-stderr", &err_rel)?;
        }
        let classified = classify(
            kind,
            &RawRun {
                exit_code: output.exit_code,
                stdout: &output.stdout,
                stderr: &output.stderr,
                timed_out: output.timed_out,
            },
            None,
        );
        let mut usage = classified.usage.clone();
        usage.wall_ms = wall_ms;
        usage.model = usage.model.or_else(|| candidate.model.clone());
        usage.agent = Some(candidate.agent.clone());
        usage.candidate_position = u32::try_from(position).ok();
        let actual_tokens = usage
            .input_tokens
            .zip(usage.output_tokens)
            .map(|(i, o)| i.saturating_add(o));
        self.append(&job.id, stage, EventPayload::AgentFinished { usage })?;
        let until = self.record_quota(claimed, candidate, &classified)?;
        if let Some(reason) =
            self.record_budget(claimed, &candidate.agent, actual_tokens, wall_ms)?
        {
            return Ok(Attempt::Settled(Verdict::NeedsHuman(reason)));
        }
        Ok(match classified.failure {
            None => Attempt::Settled(self.accept_output(claimed, run)?),
            Some(Failure::QuotaExhausted { reason, .. }) => Attempt::Quota {
                until_ms: until.unwrap_or(self.env.now_ms() + self.routing.default_backoff_ms),
                reason,
            },
            Some(Failure::ProviderError(reason)) => Attempt::Provider(reason),
            Some(Failure::Config(reason)) => Attempt::Settled(Verdict::Fail(reason)),
            Some(failure) => {
                Attempt::Settled(Verdict::Retry(retry_cause(&failure), failure.reason()))
            }
        })
    }
}
