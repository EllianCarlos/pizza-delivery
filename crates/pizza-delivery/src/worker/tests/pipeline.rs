use super::*;

#[test]
fn a_valid_handoff_completes_the_job_and_the_whole_run_is_ledgered() -> Result<()> {
    let rig = Rig::new(vec![good()])?;

    assert!(rig.worker().tick()?);

    assert_eq!(
        rig.kinds()?,
        vec![
            "job-submitted",
            "stage-planned",
            "stage-claimed",
            "stage-started",
            "agent-invoked",
            "sandbox-audit",
            "artifact-recorded",
            "agent-finished",
            "stage-budget-recorded",
            "artifact-recorded",
            "handoff-written",
            "stage-succeeded",
            "job-completed",
            "lease-released",
        ]
    );
    assert_eq!(rig.job_state()?, JobState::Completed);
    assert!(!rig.worker().tick()?);
    Ok(())
}

#[test]
fn every_invocation_records_an_audit_entry_with_all_fields() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    rig.worker().tick()?;

    let audits = rig.audits()?;
    assert_eq!(audits.len(), 1);
    let EventPayload::SandboxAudit {
        profile_hash,
        mounts,
        egress_allowlist,
        blocked_requests,
        diff_policy_result,
        pushed_ref,
        pr_url,
        ..
    } = &audits[0]
    else {
        return Err(anyhow!("not an audit"));
    };
    assert_eq!(profile_hash.len(), 64);
    assert!(mounts
        .iter()
        .any(|m| m.ends_with(":rw") && m.contains("J1")));
    assert_eq!(egress_allowlist, &vec!["api.anthropic.com".to_owned()]);
    assert!(blocked_requests.is_empty());
    assert_eq!(
        (diff_policy_result, pushed_ref, pr_url),
        (&None, &None, &None)
    );
    Ok(())
}

#[test]
fn a_failed_agent_run_is_audited_too() -> Result<()> {
    let rig = Rig::new(vec![])?;
    rig.worker().tick()?;
    assert_eq!(rig.audits()?.len(), 1);
    Ok(())
}

#[test]
fn blocked_egress_is_ledgered_per_host_and_in_the_audit() -> Result<()> {
    let rig = Rig::new(vec![good()])?
        .with_egress("api.anthropic.com\nevil.example\nevil.example\ntracker.io\n")?;
    rig.worker().tick()?;

    let blocked: Vec<String> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::EgressBlocked { host } => Some(host),
            _ => None,
        })
        .collect();
    assert_eq!(blocked, vec!["evil.example", "tracker.io"]);
    let audits = rig.audits()?;
    assert!(matches!(
        &audits[0],
        EventPayload::SandboxAudit { blocked_requests, .. }
            if blocked_requests == &vec!["evil.example".to_owned(), "tracker.io".to_owned()]
    ));
    Ok(())
}

#[test]
fn artifacts_are_indexed_by_kind_relative_path_and_sha256() -> Result<()> {
    let rig = Rig::new(vec![good()])?;

    rig.worker().tick()?;

    let artifacts: Vec<(String, String, String)> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ArtifactRecorded { kind, path, sha256 } => Some((kind, path, sha256)),
            _ => None,
        })
        .collect();
    assert_eq!(artifacts.len(), 2);
    assert_eq!(artifacts[0].0, "raw-output");
    assert_eq!(
        artifacts[0].1,
        "jobs/J1/stages/1-implement/raw-output.jsonl"
    );
    assert_eq!(
        artifacts[0].2,
        "0f3c8b90bcba63a3516fae9fd3e5d103f5c506fcc0e4fcbd7cfd280c024232a5"
    );
    assert_eq!(artifacts[1].0, "handoff");
    assert_eq!(artifacts[1].1, "jobs/J1/stages/1-implement/handoff.md");
    assert_eq!(artifacts[1].2, HANDOFF_SHA256);
    Ok(())
}

#[test]
fn an_invalid_handoff_is_retried_once_and_then_succeeds() -> Result<()> {
    let rig = Rig::new(vec![bad_handoff(), good()])?;

    rig.worker().tick()?;
    assert_eq!(rig.job_state()?, JobState::Queued);
    rig.worker().tick()?;

    let kinds = rig.kinds()?;
    assert!(kinds.contains(&"stage-retried"));
    assert!(!kinds.contains(&"stage-failed"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_handoff_that_stays_invalid_fails_the_stage_and_the_job() -> Result<()> {
    let rig = Rig::new(vec![bad_handoff(), bad_handoff()])?;

    rig.worker().tick()?;
    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    let failure = rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::StageFailed { reason } => Some(reason),
        _ => None,
    });
    assert!(failure.is_some_and(|r| r.contains("invalid handoff")));
    Ok(())
}

#[test]
fn a_missing_handoff_counts_as_invalid() -> Result<()> {
    let rig = Rig::new(vec![
        Script {
            handoff: None,
            stdout: OK_RESULT,
            ..Script::default()
        },
        good(),
    ])?;

    rig.worker().tick()?;
    rig.worker().tick()?;

    assert!(rig.kinds()?.contains(&"stage-retried"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_provider_error_fails_without_retrying_even_if_a_handoff_exists() -> Result<()> {
    let rig = Rig::new(vec![Script {
        handoff: Some(HANDOFF),
        stdout: ERR_RESULT,
        exit_code: 1,
        ..Script::default()
    }])?;

    rig.worker().tick()?;

    let kinds = rig.kinds()?;
    assert!(!kinds.contains(&"stage-retried"));
    assert!(kinds.contains(&"stage-failed"));
    assert_eq!(rig.job_state()?, JobState::Failed);
    Ok(())
}

#[test]
fn usage_is_recorded_with_unknown_values_left_empty() -> Result<()> {
    let rig = Rig::new(vec![good()])?;

    rig.worker().tick()?;

    let usage = rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::AgentFinished { usage } => Some(usage),
        _ => None,
    });
    let usage = usage.ok_or_else(|| anyhow!("no agent-finished event"))?;
    assert_eq!(usage.model, None);
    assert_eq!(usage.input_tokens, None);
    assert_eq!(usage.output_tokens, None);
    assert_eq!(usage.cost_microusd, None);
    assert_eq!(usage.wall_ms, Some(0));
    Ok(())
}

#[test]
fn an_expired_lease_is_ledgered_and_the_stage_is_reclaimed_as_attempt_two() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    let crashed = ClaimRequest {
        holder: "crashed-daemon".into(),
        now_ms: 100,
        ttl_ms: 1000,
        stage_id: "S0".into(),
        event_ids: ["EP0".into(), "EC0".into()],
        require_triage: false,
        agent: "claude".into(),
        caps: Caps::default(),
    };
    let mut guard = acquire(&rig.store, "store")?;
    guard.claim_stage(crashed)?;
    drop(guard);
    rig.env.now.store(5000, Ordering::SeqCst);

    rig.worker().tick()?;

    assert_eq!(
        rig.kinds()?[..7],
        [
            "job-submitted",
            "stage-planned",
            "stage-claimed",
            "lease-expired",
            "artifact-recorded",
            "stage-retried",
            "stage-claimed"
        ]
    );
    let attempts: Vec<u32> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::StageClaimed { attempt, .. } => Some(attempt),
            _ => None,
        })
        .collect();
    assert_eq!(attempts, vec![1, 2]);
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_lease_that_expires_on_the_last_attempt_fails_the_job() -> Result<()> {
    let rig = Rig::new(vec![])?;
    let claim = |tag: &str, now_ms| ClaimRequest {
        holder: "crashed".into(),
        now_ms,
        ttl_ms: 10,
        stage_id: format!("S{tag}"),
        event_ids: [format!("P{tag}"), format!("C{tag}")],
        require_triage: false,
        agent: "claude".into(),
        caps: Caps::default(),
    };
    let mut guard = acquire(&rig.store, "store")?;
    guard.claim_stage(claim("0", 0))?;
    drop(guard);
    rig.env.now.store(1000, Ordering::SeqCst);
    rig.worker().reap()?;
    let mut guard = acquire(&rig.store, "store")?;
    guard.claim_stage(claim("1", 1000))?;
    drop(guard);
    rig.env.now.store(5000, Ordering::SeqCst);

    rig.worker().reap()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    Ok(())
}

#[test]
fn each_tool_use_is_ledgered_as_a_small_event_before_agent_finished() -> Result<()> {
    let stdout: &'static str = Box::leak(
        format!(
            "{}\n{}\n",
            serde_json::json!({"type":"assistant","message":{"content":[
                {"type":"tool_use","name":"Read","input":{"file_path":"/a.rs"}},
                {"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}),
            OK_RESULT.trim()
        )
        .into_boxed_str(),
    );
    let rig = Rig::new(vec![Script {
        handoff: Some(HANDOFF),
        stdout,
        ..Script::default()
    }])?;

    rig.worker().tick()?;

    let calls: Vec<(String, String)> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ToolCalled { name, summary } => Some((name, summary)),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        vec![
            ("Read".to_owned(), "/a.rs".to_owned()),
            ("Bash".to_owned(), "ls".to_owned())
        ]
    );
    let kinds = rig.kinds()?;
    let finished = kinds.iter().position(|k| *k == "agent-finished");
    let last_tool = kinds.iter().rposition(|k| *k == "tool-called");
    assert!(last_tool < finished);
    Ok(())
}
