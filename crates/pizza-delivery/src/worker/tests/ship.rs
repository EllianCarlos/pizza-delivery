use super::*;

fn shipped_audit(rig: &Rig) -> Result<Option<EventPayload>> {
    Ok(rig
        .timeline()?
        .into_iter()
        .rev()
        .find_map(|e| match e.payload {
            EventPayload::SandboxAudit {
                pushed_ref: Some(_),
                ..
            } => Some(e.payload),
            _ => None,
        }))
}

#[test]
fn shipping_pushes_the_job_branch_and_opens_a_draft_pr_recorded_in_the_ledger() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    plan_of(&rig, &["implement", "ship"])?;
    *acquire(&rig.git.files, "files")? = vec!["src/a.rs".to_owned()];

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert_eq!(
        *acquire(&rig.git.pushes, "pushes")?,
        vec![("origin".to_owned(), "pizza/J1".to_owned())]
    );
    let prs = acquire(&rig.git.prs, "prs")?;
    assert_eq!(prs.len(), 1);
    assert!(prs[0].1.contains("J1") && prs[0].1.contains("## implement"));
    assert!(prs[0].2.is_empty());
    match shipped_audit(&rig)? {
        Some(EventPayload::SandboxAudit {
            pushed_ref,
            pr_url,
            diff_policy_result,
            ..
        }) => {
            assert_eq!(pushed_ref.as_deref(), Some("origin/pizza/J1"));
            assert_eq!(pr_url.as_deref(), Some("https://github.com/o/r/pull/7"));
            assert_eq!(diff_policy_result.as_deref(), Some("clean"));
        }
        other => return Err(anyhow!("no ship audit: {other:?}")),
    }
    Ok(())
}

#[test]
fn sensitive_changes_ship_as_a_labelled_draft_with_a_high_severity_risk() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    plan_of(&rig, &["implement", "ship"])?;
    *acquire(&rig.git.files, "files")? =
        vec!["src/a.rs".to_owned(), ".github/workflows/ci.yml".to_owned()];

    rig.worker().tick()?;

    let prs = acquire(&rig.git.prs, "prs")?;
    assert_eq!(prs[0].2, vec!["infra-change"]);
    assert!(prs[0].1.contains(".github/workflows/ci.yml"));
    let risks: Vec<_> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::FeedbackRecorded { kind, severity, .. } => Some((kind, severity)),
            _ => None,
        })
        .collect();
    assert_eq!(risks, vec![("risk".to_owned(), "high".to_owned())]);
    assert!(matches!(
        shipped_audit(&rig)?,
        Some(EventPayload::SandboxAudit { diff_policy_result: Some(r), .. }) if r.starts_with("sensitive")
    ));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_report_only_job_ends_with_a_report_artifact_and_never_ships() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    plan_of(&rig, &["investigate"])?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert!(acquire(&rig.git.pushes, "pushes")?.is_empty());
    assert!(acquire(&rig.git.prs, "prs")?.is_empty());
    let kinds: Vec<String> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::ArtifactRecorded { kind, .. } => Some(kind),
            _ => None,
        })
        .collect();
    assert!(kinds.contains(&"report".to_owned()));
    Ok(())
}

#[test]
fn a_rejected_push_fails_the_job_with_the_reason() -> Result<()> {
    let rig = Rig::new(vec![good()])?;
    plan_of(&rig, &["implement", "ship"])?;
    rig.git.push_fails.store(true, Ordering::SeqCst);

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert!(acquire(&rig.git.prs, "prs")?.is_empty());
    Ok(())
}

#[test]
fn the_push_remote_comes_from_the_repos_trust_config() -> Result<()> {
    let mut rig = Rig::new(vec![good()])?;
    rig.config.trust.repos.insert(
        "/src/my repo".to_owned(),
        pizza_delivery_core::sandbox::RepoTrust {
            push_remote: Some("fork".to_owned()),
            ..Default::default()
        },
    );
    plan_of(&rig, &["implement", "ship"])?;

    rig.worker().tick()?;

    assert_eq!(
        *acquire(&rig.git.pushes, "pushes")?,
        vec![("fork".to_owned(), "pizza/J1".to_owned())]
    );
    Ok(())
}
