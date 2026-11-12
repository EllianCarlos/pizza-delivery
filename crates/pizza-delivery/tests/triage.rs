mod harness;

use harness::{Harness, TestResult};

fn kinds_for(h: &Harness, id: &str) -> TestResult<Vec<String>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id)
        .map(|r| r.kind)
        .collect())
}

#[test]
fn an_explicit_repo_is_triaged_with_a_plan_context_and_context_read_events() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;
    std::fs::write(repo.join("AGENTS.md"), "conventions")?;
    let id = h.submit_in_repo("fix the flaky test")?;
    h.wait_for_end(&id)?;

    let ledger = h.ledger()?;
    let triage = ledger
        .iter()
        .find(|r| r.kind == "triage-completed")
        .ok_or("no triage-completed")?;
    let result = &triage.payload["triage"];
    assert_eq!(result["repo"]["confidence"], 100);
    assert_eq!(result["template"], "bug-fix");
    let stages: Vec<&str> = result["plan"]
        .as_array()
        .ok_or("no plan")?
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(stages, vec!["grill", "implement", "review", "ship"]);
    assert!(result["plan"][0]["reason"]
        .as_str()
        .is_some_and(|r| !r.is_empty()));
    assert!(ledger
        .iter()
        .any(|r| r.kind == "context-read" && r.payload["source"] == "conventions"));
    let context = std::fs::read_to_string(h.data_dir().join("jobs").join(&id).join("context.md"))?;
    assert!(context.contains("Sources:"));
    assert!(context.contains("AGENTS.md"));
    let kinds = kinds_for(&h, &id)?;
    let triage_at = kinds.iter().position(|k| k == "triage-completed");
    let claim_at = kinds.iter().position(|k| k == "stage-claimed");
    assert!(triage_at < claim_at);
    Ok(())
}

#[test]
fn a_project_named_in_the_prompt_is_found_in_the_index() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;
    let id = h.submit("tidy up the docs of my-repo")?;
    h.wait_for_end(&id)?;

    let ledger = h.ledger()?;
    let triage = ledger
        .iter()
        .find(|r| r.kind == "triage-completed")
        .ok_or("no triage-completed")?;
    assert_eq!(
        triage.payload["triage"]["repo"]["path"],
        repo.to_string_lossy().as_ref()
    );
    assert!(!ledger.iter().any(|r| r.kind == "feedback-recorded"));
    Ok(())
}

#[test]
fn a_low_confidence_pick_logs_an_assumption_and_still_runs() -> TestResult {
    let h = Harness::new()?;
    h.repo_named("alpha-one")?;
    h.repo_named("alpha-two")?;
    let id = h.submit("fix the alpha thing")?;
    h.wait_for_end(&id)?;

    let ledger = h.ledger()?;
    let feedback = ledger
        .iter()
        .find(|r| r.kind == "feedback-recorded")
        .ok_or("no feedback")?;
    assert_eq!(feedback.payload["kind"], "assumption");
    assert!(feedback.payload["confidence"].as_u64().is_some());
    Ok(())
}

#[test]
fn no_candidate_ends_the_job_as_needs_human_with_a_feedback_entry() -> TestResult {
    let h = Harness::new()?;
    h.repo()?;
    let id = h.submit("do something unrelated")?;

    let job = h.wait_for_state(&id, &["needs-human"])?;

    assert_eq!(job["state"], "needs-human");
    let ledger = h.ledger()?;
    let feedback = ledger
        .iter()
        .find(|r| r.kind == "feedback-recorded")
        .ok_or("no feedback")?;
    assert_eq!(feedback.payload["kind"], "needs-human");
    assert!(ledger.iter().any(|r| r.kind == "job-needs-human"));
    assert!(!ledger.iter().any(|r| r.kind == "stage-claimed"));
    Ok(())
}

#[test]
fn dry_run_prints_the_triage_and_enqueues_nothing() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;

    let cli = h.run(&[
        "--dry-run",
        "--repo",
        &repo.to_string_lossy(),
        "add a toggle",
    ])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let lines = cli.json_lines()?;
    assert_eq!(lines[0]["outcome"]["triage"]["template"], "full-feature");
    assert_eq!(lines[0]["outcome"]["triage"]["plan"][1]["name"], "spec");
    assert!(h.ledger().map(|l| l.is_empty()).unwrap_or(true));
    Ok(())
}

#[test]
fn dry_run_without_a_candidate_exits_3() -> TestResult {
    let h = Harness::new()?;

    let cli = h.run(&["--dry-run", "do something unrelated"])?;

    assert_eq!(cli.code, 3);
    assert!(h.ledger().map(|l| l.is_empty()).unwrap_or(true));
    Ok(())
}

#[test]
fn dry_run_reflects_options_and_enqueues_nothing() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;
    let cli = h.run(&[
        "--dry-run",
        "--repo",
        &repo.to_string_lossy(),
        "--template",
        "investigate",
        "--report-only",
        "--context",
        "start with the logs",
        "--risk",
        "customer-data",
        "fix the flaky test",
    ])?;
    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let line = cli.json_lines()?.into_iter().next().ok_or("no output")?;
    let triage = &line["outcome"]["triage"];
    assert_eq!(triage["template"], "investigate");
    let stages: Vec<&str> = triage["plan"]
        .as_array()
        .ok_or("no plan")?
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert_eq!(stages, vec!["investigate", "grill"]);
    assert!(triage["task"]
        .as_str()
        .is_some_and(|t| t.contains("Hint: start with the logs")));
    assert_eq!(triage["risk_flags"][0], "customer-data");
    assert!(line["outcome"]["feedback"].as_array().is_some_and(|f| f
        .iter()
        .any(|x| x["kind"] == "assumption" && x["severity"] == "low")));
    assert!(h.ledger()?.is_empty());
    Ok(())
}

#[test]
fn dry_run_rejects_invalid_options() -> TestResult {
    let h = Harness::new()?;
    let cli = h.run(&["--dry-run", "--max-stages", "0", "x"])?;
    assert_eq!(cli.code, 2, "stderr: {}", cli.stderr);
    Ok(())
}

#[test]
fn a_submitted_jobs_plan_honours_report_only_and_max_stages() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;
    let cli = h.run(&[
        "--repo",
        &repo.to_string_lossy(),
        "--report-only",
        "--max-stages",
        "1",
        "fix the flaky test",
    ])?;
    let id = cli.json_lines()?[0]["id"]
        .as_str()
        .ok_or("no id")?
        .to_owned();
    h.wait_for_end(&id)?;
    let ledger = h.ledger()?;
    let triage = ledger
        .iter()
        .find(|r| r.kind == "triage-completed")
        .ok_or("no triage-completed")?;
    let plan = triage.payload["triage"]["plan"]
        .as_array()
        .ok_or("no plan")?;
    assert_eq!(plan.len(), 1);
    assert_eq!(plan[0]["name"], "grill");
    Ok(())
}
