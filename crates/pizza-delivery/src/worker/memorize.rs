use super::*;

impl Worker<'_> {
    pub(super) fn memorize_if_due(&self, job: &JobId) {
        if self.config.memory.is_none() {
            return;
        }
        let Ok(events) = self.with_store(|s| s.read_timeline(job)) else {
            return;
        };
        if !should_memorize(&events) {
            return;
        }
        if let Err(e) = self.memorize(job, &events) {
            let _ = self.append(
                job,
                None,
                EventPayload::FeedbackRecorded {
                    kind: "risk".to_owned(),
                    severity: "low".to_owned(),
                    text: format!(
                        "The memorize stage failed and nothing may have been saved: {e:#}"
                    ),
                    confidence: None,
                    alternatives: vec![],
                    impact: Some("the job result is unaffected".to_owned()),
                },
            );
        }
    }

    pub(super) fn memory_cli(&self, store: MemoryStore, args: &[String]) -> Result<String> {
        let setup = self
            .config
            .memory
            .as_ref()
            .ok_or_else(|| anyhow!("memory is not configured"))?;
        let command = match store {
            MemoryStore::Engram => &setup.engram,
            MemoryStore::Mnemon => &setup.mnemon,
        };
        let output = std::process::Command::new(command).args(args).output()?;
        if !output.status.success() {
            return Err(anyhow!(
                "{} {}: {}",
                store.as_str(),
                args.first().map_or("", String::as_str),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    pub(super) fn memory_search(
        &self,
        store: MemoryStore,
        query: &str,
        project: &str,
    ) -> Result<Vec<pizza_delivery_core::memorize::Existing>> {
        let verb = if store == MemoryStore::Engram {
            "search"
        } else {
            "recall"
        };
        let mut args = vec![
            verb.to_owned(),
            query.to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
        ];
        if store == MemoryStore::Engram {
            args.extend(["--project".to_owned(), project.to_owned()]);
        }
        Ok(parse_existing(&self.memory_cli(store, &args)?))
    }

    pub(super) fn memory_write(
        &self,
        entry: &Entry,
        decision: &MemoryDecision,
        job: &JobId,
        project: &str,
    ) -> Result<(String, String)> {
        let store = entry.kind.store();
        let tags = [
            "--tag".to_owned(),
            OWN_TAG.to_owned(),
            "--tag".to_owned(),
            job_tag(job.as_str()),
        ];
        let mut args = match decision {
            MemoryDecision::Update { id } => vec![
                "update".to_owned(),
                id.clone(),
                entry.topic.clone(),
                entry.content.clone(),
            ],
            _ => vec![
                if store == MemoryStore::Engram {
                    "save"
                } else {
                    "remember"
                }
                .to_owned(),
                entry.topic.clone(),
                entry.content.clone(),
            ],
        };
        args.extend(tags);
        if store == MemoryStore::Engram {
            args.extend(["--project".to_owned(), project.to_owned()]);
        }
        let out = self.memory_cli(store, &args)?;
        Ok(match decision {
            MemoryDecision::Update { id } => (id.clone(), "updated".to_owned()),
            _ => (out.trim().to_owned(), "saved".to_owned()),
        })
    }

    pub(super) fn memorize(&self, job: &JobId, events: &[Event]) -> Result<()> {
        let record = self.find_job(job)?.ok_or_else(|| anyhow!("job vanished"))?;
        let repo = record.repo.as_deref().map(PathBuf::from);
        let project = repo
            .as_deref()
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let confirmed = events.iter().any(|e| {
            matches!(
                e.payload,
                EventPayload::JobSubmitted {
                    follow_up: Some(_),
                    ..
                }
            )
        });
        let outcome = events
            .iter()
            .rev()
            .find_map(|e| match &e.payload {
                EventPayload::JobCompleted {} => Some("completed".to_owned()),
                EventPayload::JobFailed { reason } => Some(format!("failed: {reason}")),
                EventPayload::JobNeedsHuman { reason } => Some(format!("needs human: {reason}")),
                _ => None,
            })
            .unwrap_or_default();
        let relative = stage_dir(job, 0, "memorize");
        let dir = self.config.data_dir.join(&relative);
        std::fs::create_dir_all(&dir)?;
        let plan_path = dir.join("plan.jsonl");
        if plan_path.exists() {
            std::fs::remove_file(&plan_path)?;
        }
        let text = memorize_prompt(
            job.as_str(),
            &record.prompt,
            &outcome,
            &self.handoffs(events),
            &plan_path.to_string_lossy(),
        );
        let quota = fold_quota(&self.with_store(|s| s.quota_events())?);
        let routed = route(
            self.routing,
            &RouteRequest {
                stage: "memorize",
                tier: DEFAULT_TIER,
                repo: record.repo.as_deref(),
                required: &[],
            },
            &quota,
            self.env.now_ms(),
        )
        .map_err(|e| anyhow!("routing: {e}"))?;
        let egress_log = dir.join("egress-requests.log");
        let sandbox = self.config.trust.profile_for(
            &repo.as_deref().unwrap_or(Path::new("")).to_string_lossy(),
            &[dir.to_string_lossy().into_owned()],
        );
        let mut ran = false;
        for candidate in &routed.candidates {
            let Some((_, agent)) = self.fleet.get(&candidate.agent) else {
                continue;
            };
            self.append(
                job,
                None,
                EventPayload::AgentInvoked {
                    agent: candidate.agent.clone(),
                    template: "memorize".to_owned(),
                    version: 1,
                    prompt_sha256: sha256_hex(text.as_bytes()),
                },
            )?;
            let invocation = Invocation {
                prompt: text.clone(),
                model: candidate.model.clone(),
                cwd: dir.clone(),
                handoff_path: plan_path.clone(),
                timeout: self
                    .config
                    .timeout
                    .unwrap_or_else(|| stage_timeout("memorize", tier_of("memorize"))),
                grace: self.config.kill_grace,
                sandbox: sandbox.clone(),
                egress_log: egress_log.clone(),
                mcp_config: None,
            };
            if agent.run(&invocation, &mut |_| {}).is_ok() {
                ran = true;
                break;
            }
        }
        if !ran {
            return Err(anyhow!("no agent could run the memorize stage"));
        }
        let entries = parse_plan(&std::fs::read_to_string(&plan_path).unwrap_or_default());
        let mut applied: Vec<(MemoryStore, String, String, String)> = Vec::new();
        for entry in &entries {
            let store = entry.kind.store();
            let existing = self
                .memory_search(store, &entry.topic, &project)
                .unwrap_or_default();
            let decision = decide(entry, &existing, confirmed);
            let (id, action) = match &decision {
                MemoryDecision::Save | MemoryDecision::Update { .. } => self
                    .memory_write(entry, &decision, job, &project)
                    .unwrap_or_else(|e| (String::new(), format!("failed: {e:#}"))),
                MemoryDecision::Unchanged { id } => (id.clone(), "unchanged".to_owned()),
                MemoryDecision::Contradiction { id } => {
                    let risk = contradiction_risk(entry, store, id);
                    self.append(
                        job,
                        None,
                        EventPayload::FeedbackRecorded {
                            kind: risk.kind,
                            severity: risk.severity,
                            text: risk.text,
                            confidence: risk.confidence,
                            alternatives: risk.alternatives,
                            impact: risk.impact,
                        },
                    )?;
                    (id.clone(), "contradiction".to_owned())
                }
                MemoryDecision::Unconfirmed => (String::new(), "skipped-unconfirmed".to_owned()),
            };
            applied.push((store, id, entry.topic.clone(), action));
        }
        let tag = job_tag(job.as_str());
        let mut ledger: Vec<(MemoryStore, String, String, String)> = Vec::new();
        for store in [MemoryStore::Engram, MemoryStore::Mnemon] {
            for found in self
                .memory_search(store, &tag, &project)
                .unwrap_or_default()
            {
                if !found.has_tag(&tag) {
                    continue;
                }
                let action = applied
                    .iter()
                    .find(|(s, id, _, _)| *s == store && *id == found.id)
                    .map_or("found", |(_, _, _, a)| a.as_str());
                ledger.push((store, found.id, found.topic, action.to_owned()));
            }
        }
        for item in applied {
            if !ledger
                .iter()
                .any(|(s, id, _, _)| *s == item.0 && *id == item.1)
            {
                ledger.push(item);
            }
        }
        let mut listing = String::new();
        for (store, id, topic, action) in ledger {
            listing.push_str(&format!(
                "{}\n",
                serde_json::json!({"store": store.as_str(), "id": id, "topic": topic, "action": action})
            ));
            self.append(
                job,
                None,
                EventPayload::WriteBack {
                    store: store.as_str().to_owned(),
                    id,
                    topic,
                    action,
                },
            )?;
        }
        let listing_rel = relative.join("memory-writeback.jsonl");
        std::fs::write(self.config.data_dir.join(&listing_rel), listing)?;
        self.record_file(job, None, ARTIFACT_KIND, &listing_rel)
    }
}
