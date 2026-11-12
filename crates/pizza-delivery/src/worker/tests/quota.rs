use super::*;

const QUOTA_WITH_RESET: &str = "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\",\"resetsAt\":10}}\n{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429}\n";

fn exhausted(stdout: &'static str) -> Script {
    Script {
        handoff: None,
        stdout,
        exit_code: 1,
        ..Script::default()
    }
}

fn invoked(rig: &Rig) -> Result<Vec<String>> {
    Ok(rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AgentInvoked { agent, .. } => Some(agent),
            _ => None,
        })
        .collect())
}

#[test]
fn quota_exhaustion_falls_back_to_the_next_candidate_with_an_interruption_note() -> Result<()> {
    let rig = Rig::with_chain(
        vec![
            ("claude", AdapterKind::Claude, vec![exhausted(QUOTA_429)]),
            ("claude-alt", AdapterKind::Claude, vec![good()]),
        ],
        CLAUDE_ALT_OWNER,
    )?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert_eq!(invoked(&rig)?, ["claude", "claude-alt"]);
    let kinds = rig.kinds()?;
    assert!(kinds.contains(&"quota-signal"));
    assert!(kinds.contains(&"agent-fell-back"));
    assert!(!kinds.contains(&"stage-retried"));
    let stage_dir = rig._dir.path().join("jobs/J1/stages/1-implement");
    let first = std::fs::read_to_string(stage_dir.join("prompt-claude.txt"))?;
    let second = std::fs::read_to_string(stage_dir.join("prompt-claude-alt.txt"))?;
    assert!(!first.contains("Interruption note"));
    assert!(second.contains("Interruption note"));
    Ok(())
}

#[test]
fn a_fully_exhausted_chain_parks_the_job_until_the_earliest_known_reset_and_resumes_on_its_own(
) -> Result<()> {
    let rig = Rig::with_chain(
        vec![
            (
                "claude",
                AdapterKind::Claude,
                vec![exhausted(QUOTA_WITH_RESET), good()],
            ),
            (
                "claude-alt",
                AdapterKind::Claude,
                vec![exhausted(
                    "{\"type\":\"error\",\"error\":{\"message\":\"quota exceeded\"}}\n",
                )],
            ),
        ],
        CLAUDE_ALT_OWNER,
    )?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::WaitingForQuota);
    let parked = rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::JobParked { until_ms, .. } => Some(until_ms),
        _ => None,
    });
    assert_eq!(parked, Some(10_000));
    assert!(!rig.worker().tick()?);
    rig.env.now.store(9_999, Ordering::SeqCst);
    assert!(!rig.worker().tick()?);

    rig.env.now.store(10_000, Ordering::SeqCst);
    assert!(rig.worker().tick()?);

    assert_eq!(rig.job_state()?, JobState::Completed);
    let attempts: Vec<u32> = fold_stages(&rig.timeline()?)
        .iter()
        .map(|s| s.attempt)
        .collect();
    assert_eq!(attempts, [1]);
    Ok(())
}

#[test]
fn exhaustion_without_a_known_reset_parks_for_the_default_backoff() -> Result<()> {
    let rig = Rig::new(vec![exhausted(QUOTA_429)])?;

    rig.worker().tick()?;

    let timeline = rig.timeline()?;
    let signal = timeline.iter().find_map(|e| match &e.payload {
        EventPayload::QuotaSignal {
            reset_known,
            resets_at_ms,
            ..
        } => Some((*reset_known, *resets_at_ms)),
        _ => None,
    });
    assert_eq!(signal, Some((false, Some(100 + 3_600_000))));
    assert_eq!(rig.job_state()?, JobState::WaitingForQuota);
    Ok(())
}

#[test]
fn a_candidate_known_to_be_exhausted_is_skipped_without_being_run() -> Result<()> {
    let rig = Rig::with_chain(
        vec![
            (
                "claude",
                AdapterKind::Claude,
                vec![exhausted(QUOTA_WITH_RESET)],
            ),
            ("claude-alt", AdapterKind::Claude, vec![good(), good()]),
        ],
        CLAUDE_ALT_OWNER,
    )?;
    rig.worker().tick()?;
    rig.with_store(|s| {
        s.enqueue_job(NewJob {
            id: JobId::new("J2"),
            event_id: "E2".into(),
            prompt: "second".into(),
            repo: Some("/src/my repo".into()),
            priority: Priority::Normal,
            follow_up: None,
            options: None,
            submitted_at_ms: 0,
        })
        .map(|_| ())
    })?;
    rig.env.now.store(5_000, Ordering::SeqCst);

    rig.worker().tick()?;

    let second: Vec<String> = rig
        .with_store(|s| s.read_timeline(&JobId::new("J2")))?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::AgentInvoked { agent, .. } => Some(agent),
            _ => None,
        })
        .collect();
    assert_eq!(second, ["claude-alt"]);
    Ok(())
}

#[test]
fn a_configuration_error_fails_fast_without_retry_or_fallback() -> Result<()> {
    let rig = Rig::with_chain(
        vec![
            (
                "hermes",
                AdapterKind::Hermes,
                vec![Script {
                    handoff: None,
                    stdout: "",
                    stderr: "No usable credentials found for provider 'gemini'.",
                    exit_code: 1,
                    timed_out: false,
                    review: None,
                }],
            ),
            ("claude", AdapterKind::Claude, vec![good()]),
        ],
        "[[rules]]\nchain = [{ agent = \"hermes\" }, { agent = \"claude\" }]\n[agents.hermes]\nadapter = \"hermes\"\ncommand = \"hermes\"\n",
    )?;
    plan_of(&rig, &["investigate"])?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert_eq!(invoked(&rig)?, ["hermes"]);
    let kinds = rig.kinds()?;
    assert!(!kinds.contains(&"stage-retried"));
    assert!(!kinds.contains(&"agent-fell-back"));
    Ok(())
}

#[test]
fn terminal_provider_errors_fall_back_and_fail_the_job_when_the_chain_runs_out() -> Result<()> {
    let rig = Rig::with_chain(
        vec![
            ("claude", AdapterKind::Claude, vec![exhausted(ERR_RESULT)]),
            (
                "claude-alt",
                AdapterKind::Claude,
                vec![exhausted(ERR_RESULT)],
            ),
        ],
        CLAUDE_ALT_OWNER,
    )?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert_eq!(invoked(&rig)?, ["claude", "claude-alt"]);
    assert!(!rig.kinds()?.contains(&"job-parked"));
    Ok(())
}

#[test]
fn a_healthy_rate_limit_event_is_ledgered_as_an_allowed_quota_signal() -> Result<()> {
    let rig = Rig::new(vec![Script {
        handoff: Some(HANDOFF),
        stdout: "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"allowed\",\"resetsAt\":50,\"unifiedWindows\":{\"five_hour\":{\"utilization\":0.95,\"resetsAt\":50}}}}\n{\"type\":\"result\",\"is_error\":false}\n",
        stderr: "",
        exit_code: 0,
        timed_out: false,
        review: None,
    }])?;

    rig.worker().tick()?;

    let signal = rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::QuotaSignal {
            status,
            utilization_permille,
            ..
        } => Some((status, utilization_permille)),
        _ => None,
    });
    assert_eq!(signal, Some((QuotaStatus::Allowed, Some(950))));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}
