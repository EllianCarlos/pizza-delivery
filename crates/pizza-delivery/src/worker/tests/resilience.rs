use super::*;

fn hung() -> Script {
    Script {
        timed_out: true,
        ..Script::default()
    }
}

fn flaky() -> Script {
    Script::default()
}

#[test]
fn a_timeout_commits_wip_writes_a_note_and_is_retried_once_then_fails() -> Result<()> {
    let rig = Rig::new(vec![hung(), hung()])?;

    rig.worker().tick()?;
    assert_eq!(rig.job_state()?, JobState::Queued);
    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert_eq!(retry_causes(&rig)?, vec![RetryCause::Timeout]);
    assert_eq!(wip_count(&rig)?, 2);
    let note = std::fs::read_to_string(
        rig.config
            .data_dir
            .join("jobs/J1/stages/1-implement/interruption-1.md"),
    )?;
    assert!(note.contains("timeout"));
    assert!(note.contains("wip1"));
    assert!(rig.kinds()?.contains(&"artifact-recorded"));
    Ok(())
}

#[test]
fn the_attempt_after_an_interruption_is_given_the_note() -> Result<()> {
    let rig = Rig::new(vec![hung(), good()])?;

    rig.worker().tick()?;
    rig.worker().tick()?;

    let prompts = rig.prompts()?;
    assert!(!prompts[0].contains("Interruption note"));
    assert!(prompts[1].contains("Interruption note"));
    assert!(prompts[1].contains("wip1"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn transient_failures_are_retried_twice() -> Result<()> {
    let rig = Rig::new(vec![flaky(), flaky(), good()])?;

    for _ in 0..3 {
        rig.worker().tick()?;
    }

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert_eq!(
        retry_causes(&rig)?,
        vec![RetryCause::Transient, RetryCause::Transient]
    );
    assert_eq!(wip_count(&rig)?, 0);
    Ok(())
}

#[test]
fn a_third_transient_failure_is_final() -> Result<()> {
    let rig = Rig::new(vec![flaky(), flaky(), flaky()])?;

    for _ in 0..3 {
        rig.worker().tick()?;
    }

    assert_eq!(rig.job_state()?, JobState::Failed);
    Ok(())
}

#[test]
fn a_crash_leaves_a_note_and_a_wip_commit_for_the_next_attempt() -> Result<()> {
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
    acquire(&rig.store, "store")?.claim_stage(crashed)?;
    rig.env.now.store(5000, Ordering::SeqCst);

    rig.worker().tick()?;

    let note = std::fs::read_to_string(
        rig.config
            .data_dir
            .join("jobs/J1/stages/1-implement/interruption-1.md"),
    )?;
    assert!(note.contains("crash"));
    assert_eq!(wip_count(&rig)?, 1);
    assert_eq!(retry_causes(&rig)?, vec![RetryCause::Crash]);
    let prompts = rig.prompts()?;
    assert!(prompts[0].contains("Interruption note"));
    Ok(())
}

fn completed_rig(pushed: bool, dirty: bool) -> Result<Rig> {
    let rig = Rig::new(vec![good()])?;
    rig.worker().tick()?;
    rig.git.pushed.store(pushed, Ordering::SeqCst);
    rig.git.dirty.store(dirty, Ordering::SeqCst);
    Ok(rig)
}

fn removed(rig: &Rig) -> Result<usize> {
    Ok(acquire(&rig.git.removed, "removed")?.len())
}

#[test]
fn a_clean_pushed_worktree_is_removed_after_retention() -> Result<()> {
    let rig = completed_rig(true, false)?;
    rig.env.now.store(5000, Ordering::SeqCst);

    assert_eq!(rig.worker().sweep()?, 1);
    assert_eq!(removed(&rig)?, 1);
    Ok(())
}

#[test]
fn a_worktree_inside_the_retention_period_is_kept() -> Result<()> {
    let rig = completed_rig(true, false)?;
    rig.env.now.store(500, Ordering::SeqCst);

    assert_eq!(rig.worker().sweep()?, 0);
    Ok(())
}

#[test]
fn dirty_or_unpushed_worktrees_are_never_removed() -> Result<()> {
    for (pushed, dirty) in [(true, true), (false, false), (false, true)] {
        let rig = completed_rig(pushed, dirty)?;
        rig.env.now.store(99_999_999, Ordering::SeqCst);

        assert_eq!(rig.worker().sweep()?, 0);
        assert_eq!(removed(&rig)?, 0);
    }
    Ok(())
}

#[test]
fn a_failed_jobs_worktree_is_never_removed() -> Result<()> {
    let rig = Rig::new(vec![Script {
        handoff: Some(HANDOFF),
        stdout: ERR_RESULT,
        exit_code: 1,
        ..Script::default()
    }])?;
    rig.worker().tick()?;
    rig.git.pushed.store(true, Ordering::SeqCst);
    rig.env.now.store(99_999_999, Ordering::SeqCst);

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert_eq!(rig.worker().sweep()?, 0);
    Ok(())
}
