mod harness;

use harness::{Harness, LedgerRow, TestResult};

fn harness() -> TestResult<Harness> {
    let script = harness::VALID_HANDOFF_AGENT.replace(
        "## Assumptions\nnone",
        "## Assumptions\n- The API stays v1 [confidence: 70; alternatives: bump to v2; impact: clients break]",
    );
    Harness::with_agent(&script)
}

fn finished_job(h: &Harness) -> TestResult<String> {
    let id = h.submit_in_repo("build the thing")?;
    let job = h.wait_for_end(&id)?;
    assert_eq!(job["state"], "completed");
    Ok(id)
}

fn inbox(h: &Harness, extra: &[&str]) -> TestResult<Vec<serde_json::Value>> {
    let mut args = vec!["inbox"];
    args.extend_from_slice(extra);
    let cli = h.run(&args)?;
    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    cli.json_lines()
}

fn entry_ids(h: &Harness) -> TestResult<Vec<String>> {
    Ok(inbox(h, &[])?
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .collect())
}

fn rows_of(h: &Harness, id: &str) -> TestResult<Vec<(i64, String)>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r: &LedgerRow| r.job_id == id)
        .map(|r| (r.seq, r.kind))
        .collect())
}

fn follow_ups(h: &Harness) -> TestResult<Vec<LedgerRow>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.kind == "job-submitted" && r.payload.get("follow_up").is_some())
        .collect())
}

#[test]
fn the_inbox_lists_feedback_entries_with_their_job_and_state() -> TestResult {
    let h = harness()?;
    let job = finished_job(&h)?;

    let entries = inbox(&h, &[])?;

    assert!(entries.len() >= 2);
    for entry in &entries {
        assert_eq!(entry["job"], job.as_str());
        assert_eq!(entry["kind"], "assumption");
        assert_eq!(entry["text"], "The API stays v1");
        assert_eq!(entry["status"], "open");
        assert_eq!(entry["answered_by"], serde_json::Value::Null);
    }
    Ok(())
}

#[test]
fn inbox_show_prints_one_entry_in_full() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let id = entry_ids(&h)?.remove(0);

    let cli = h.run(&["inbox", "show", &id])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let lines = cli.json_lines()?;
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["id"], id.as_str());
    assert_eq!(lines[0]["confidence"], 70);
    assert_eq!(lines[0]["alternatives"], serde_json::json!(["bump to v2"]));
    assert_eq!(lines[0]["impact"], "clients break");
    Ok(())
}

#[test]
fn replying_to_a_job_creates_exactly_one_follow_up_and_leaves_the_original_alone() -> TestResult {
    let h = harness()?;
    let job = finished_job(&h)?;
    let before = rows_of(&h, &job)?;

    let cli = h.run(&["reply", &job, "-m", "keep v1 but add a flag"])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let reply = cli.json_lines()?.remove(0);
    let follow_up = reply["id"].as_str().ok_or("no follow-up id")?.to_owned();
    let submitted = follow_ups(&h)?;
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0].job_id, follow_up);
    assert_eq!(submitted[0].payload["priority"], "high");
    assert_eq!(submitted[0].payload["follow_up"]["parent"], job.as_str());
    let prompt = submitted[0].payload["prompt"].as_str().ok_or("no prompt")?;
    assert!(prompt.contains("build the thing"));
    assert!(prompt.contains("The API stays v1"));
    assert!(prompt.contains("keep v1 but add a flag"));
    assert_eq!(rows_of(&h, &job)?, before);
    Ok(())
}

#[test]
fn answered_entries_reference_the_follow_up_job() -> TestResult {
    let h = harness()?;
    let job = finished_job(&h)?;
    let total = entry_ids(&h)?.len();

    let reply = h
        .run(&["reply", &job, "-m", "go on"])?
        .json_lines()?
        .remove(0);

    let follow_up = reply["id"].as_str().ok_or("no follow-up id")?;
    let entries = inbox(&h, &[])?
        .into_iter()
        .filter(|e| e["job"] == job.as_str())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), total);
    for entry in entries {
        assert_eq!(entry["status"], "answered");
        assert_eq!(entry["answered_by"], follow_up);
    }
    Ok(())
}

#[test]
fn a_reply_answers_only_the_entries_it_names() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let ids = entry_ids(&h)?;
    assert!(ids.len() >= 2);

    let reply = h
        .run(&["reply", &ids[0], "-m", "about this one"])?
        .json_lines()?
        .remove(0);

    let follow_up = reply["id"].as_str().ok_or("no follow-up id")?;
    let entries = inbox(&h, &[])?;
    let status_of = |id: &str| {
        entries
            .iter()
            .find(|e| e["id"] == id)
            .map(|e| (e["status"].clone(), e["answered_by"].clone()))
    };
    assert_eq!(
        status_of(&ids[0]),
        Some(("answered".into(), follow_up.into()))
    );
    assert_eq!(
        status_of(&ids[1]),
        Some(("open".into(), serde_json::Value::Null))
    );
    assert_eq!(follow_ups(&h)?.len(), 1);
    Ok(())
}

#[test]
fn dismissing_an_entry_hides_it_and_creates_no_job() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let ids = entry_ids(&h)?;
    let jobs_before = h.run(&["status"])?.json_lines()?.len();

    let cli = h.run(&["inbox", "dismiss", &ids[0]])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    assert!(!entry_ids(&h)?.contains(&ids[0]));
    let all = inbox(&h, &["--all"])?;
    let dismissed = all.iter().find(|e| e["id"] == ids[0].as_str());
    assert_eq!(
        dismissed.map(|e| e["status"].clone()),
        Some("dismissed".into())
    );
    assert_eq!(h.run(&["status"])?.json_lines()?.len(), jobs_before);
    assert!(follow_ups(&h)?.is_empty());
    Ok(())
}

#[test]
fn a_dismissed_entry_cannot_be_replied_to() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let ids = entry_ids(&h)?;
    h.run(&["inbox", "dismiss", &ids[0]])?;

    let cli = h.run(&["reply", &ids[0], "-m", "too late"])?;

    assert_eq!(cli.code, 2);
    assert!(follow_ups(&h)?.is_empty());
    Ok(())
}

#[test]
fn a_follow_up_branches_from_the_parent_tip() -> TestResult {
    let h = harness()?;
    let job = finished_job(&h)?;
    let repo = h.repo()?;
    let parent_branch = format!("pizza/{job}");
    let tree = h.vcs(&repo, &["rev-parse", &format!("{parent_branch}^{{tree}}")])?;
    let tip = h.vcs(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit-tree",
            &tree,
            "-p",
            &parent_branch,
            "-m",
            "parent work",
        ],
    )?;
    h.vcs(
        &repo,
        &["update-ref", &format!("refs/heads/{parent_branch}"), &tip],
    )?;

    let reply = h
        .run(&["reply", &job, "-m", "continue"])?
        .json_lines()?
        .remove(0);
    let follow_up = reply["id"].as_str().ok_or("no follow-up id")?;
    h.wait_for_end(follow_up)?;

    let ancestry = h.vcs(
        &repo,
        &[
            "merge-base",
            "--is-ancestor",
            &tip,
            &format!("pizza/{follow_up}"),
        ],
    );
    assert!(ancestry.is_ok(), "{ancestry:?}");
    Ok(())
}

#[test]
fn omitting_the_text_opens_the_editor() -> TestResult {
    let editor = "#!/bin/sh\nprintf 'typed in the editor\\n' > \"$1\"\n";
    let h = harness()?;
    let path = h.root().join("editor");
    std::fs::write(&path, editor)?;
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))?;
    let h = h
        .with_env("VISUAL", &path.to_string_lossy())
        .with_env("EDITOR", &path.to_string_lossy());
    let job = finished_job(&h)?;

    let cli = h.run(&["reply", &job])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let submitted = follow_ups(&h)?;
    assert_eq!(submitted.len(), 1);
    let prompt = submitted[0].payload["prompt"].as_str().ok_or("no prompt")?;
    assert!(prompt.contains("typed in the editor"));
    Ok(())
}
