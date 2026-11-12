use super::*;

const PR_URL: &str = "https://github.com/o/r/pull/7";

fn merge_events(rig: &Rig) -> Result<Vec<EventPayload>> {
    Ok(rig
        .timeline()?
        .into_iter()
        .map(|e| e.payload)
        .filter(|p| {
            matches!(
                p,
                EventPayload::PrMerged { .. } | EventPayload::PrNotMerged { .. }
            )
        })
        .collect())
}

fn not_merged_reason(rig: &Rig) -> Result<String> {
    match merge_events(rig)?.pop() {
        Some(EventPayload::PrNotMerged { reason, .. }) => Ok(reason),
        other => Err(anyhow!("expected a not-merged event, got {other:?}")),
    }
}

fn run_reviewed(rig: &Rig) -> Result<()> {
    plan_of(rig, &["implement", "review", "ship"])?;
    for _ in 0..3 {
        rig.worker().tick()?;
    }
    Ok(())
}

fn reviewed_rig() -> Result<Rig> {
    Rig::new(vec![good(), good_review()])
}

#[test]
fn a_reviewed_clean_job_has_its_pr_merged_and_the_ledger_says_so() -> Result<()> {
    let rig = reviewed_rig()?;
    *acquire(&rig.git.files, "files")? = vec!["src/a.rs".to_owned()];

    run_reviewed(&rig)?;

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert_eq!(
        *acquire(&rig.git.merges, "merges")?,
        vec![PR_URL.to_owned()]
    );
    assert_eq!(
        merge_events(&rig)?,
        vec![EventPayload::PrMerged {
            pr_url: PR_URL.to_owned()
        }]
    );
    Ok(())
}

#[test]
fn infrastructure_changes_stay_a_draft_and_are_not_merged() -> Result<()> {
    let rig = reviewed_rig()?;
    *acquire(&rig.git.files, "files")? = vec![".github/workflows/ci.yml".to_owned()];

    run_reviewed(&rig)?;

    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert!(acquire(&rig.git.ready_checks, "ready_checks")?.is_empty());
    assert!(not_merged_reason(&rig)?.contains(".github/workflows/ci.yml"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_pr_that_is_not_ready_is_left_open_with_the_reason_recorded() -> Result<()> {
    let rig = reviewed_rig()?;
    *acquire(&rig.git.blocked, "blocked")? = Some("checks failing: ci".to_owned());

    run_reviewed(&rig)?;

    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert_eq!(not_merged_reason(&rig)?, "checks failing: ci");
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_failing_readiness_check_never_fails_the_job() -> Result<()> {
    let rig = reviewed_rig()?;
    rig.git.readiness_fails.store(true, Ordering::SeqCst);

    run_reviewed(&rig)?;

    assert!(not_merged_reason(&rig)?.contains("gh unavailable"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_rejected_merge_never_fails_the_job() -> Result<()> {
    let rig = reviewed_rig()?;
    rig.git.merge_fails.store(true, Ordering::SeqCst);

    run_reviewed(&rig)?;

    assert!(not_merged_reason(&rig)?.contains("merge rejected"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn the_auto_merge_switch_turns_merging_off() -> Result<()> {
    let mut rig = reviewed_rig()?;
    rig.config.auto_merge = false;

    run_reviewed(&rig)?;

    assert!(acquire(&rig.git.ready_checks, "ready_checks")?.is_empty());
    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert!(not_merged_reason(&rig)?.contains("auto-merge is disabled"));
    assert_eq!(acquire(&rig.git.prs, "prs")?.len(), 1);
    Ok(())
}

#[test]
fn a_job_without_a_review_stage_is_not_merged() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    plan_of(&rig, &["implement", "ship"])?;

    rig.worker().tick()?;
    rig.worker().tick()?;

    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert!(not_merged_reason(&rig)?.contains("review"));
    Ok(())
}

#[test]
fn a_review_that_still_has_blocking_findings_does_not_merge() -> Result<()> {
    let scripts = std::iter::once(good())
        .chain((0..8).flat_map(|_| [blocking_review(), good()]))
        .collect();
    let rig = Rig::new(scripts)?;
    plan_of(&rig, &["implement", "review", "ship"])?;

    for _ in 0..30 {
        rig.worker().tick()?;
    }

    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert!(not_merged_reason(&rig)?.contains("blocking"));
    Ok(())
}

fn parent_audit(pr_url: Option<&str>) -> EventPayload {
    EventPayload::SandboxAudit {
        profile_hash: "daemon-ship".into(),
        mounts: vec![],
        egress_allowlist: vec![],
        blocked_requests: vec![],
        mcp_activations: vec![],
        diff_policy_result: Some("clean".into()),
        pushed_ref: Some("origin/pizza/J0".into()),
        pr_url: pr_url.map(str::to_owned),
    }
}

fn follow_up_rig(parent_events: Vec<EventPayload>) -> Result<Rig> {
    let rig = Rig::build(
        vec![("claude", AdapterKind::Claude, vec![good(), good_review()])],
        "",
        Some(FollowUp {
            parent: JobId::new("J0"),
            entries: vec![],
            reply: "r".into(),
        }),
    )?;
    {
        let mut store = acquire(&rig.store, "store")?;
        store.enqueue_job(NewJob {
            id: JobId::new("J0"),
            event_id: "E00".into(),
            prompt: "parent".into(),
            repo: Some("/src/my repo".into()),
            priority: Priority::Normal,
            follow_up: None,
            options: None,
            submitted_at_ms: 0,
        })?;
    }
    for payload in parent_events {
        rig.worker().append(&JobId::new("J0"), None, payload)?;
    }
    rig.worker()
        .append(&JobId::new("J0"), None, EventPayload::JobCompleted {})?;
    Ok(rig)
}

#[test]
fn a_follow_up_waits_for_its_parents_pr_to_merge_first() -> Result<()> {
    let rig = follow_up_rig(vec![parent_audit(Some("https://github.com/o/r/pull/3"))])?;

    run_reviewed(&rig)?;

    assert!(acquire(&rig.git.merges, "merges")?.is_empty());
    assert!(not_merged_reason(&rig)?.contains("J0"));
    Ok(())
}

#[test]
fn a_follow_up_merges_once_its_parents_pr_is_merged() -> Result<()> {
    let rig = follow_up_rig(vec![
        parent_audit(Some("https://github.com/o/r/pull/3")),
        EventPayload::PrMerged {
            pr_url: "https://github.com/o/r/pull/3".into(),
        },
    ])?;

    run_reviewed(&rig)?;

    assert_eq!(
        *acquire(&rig.git.merges, "merges")?,
        vec![PR_URL.to_owned()]
    );
    Ok(())
}
