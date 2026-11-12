mod harness;

use harness::{Harness, TestResult};

#[test]
fn submit_returns_a_short_id_and_records_the_job_in_the_ledger() -> TestResult {
    let h = Harness::new()?;

    let cli = h.run(&["fix the flaky test"])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let lines = cli.json_lines()?;
    assert_eq!(lines.len(), 1);
    let id = lines[0]["id"].as_str().ok_or("no id")?;
    let short = lines[0]["short_id"].as_str().ok_or("no short_id")?;
    assert_eq!(id.len(), 26);
    assert!(id.starts_with(short));
    assert!(short.len() < id.len());

    let ledger = h.ledger()?;
    assert_eq!(ledger[0].job_id, id);
    assert_eq!(ledger[0].seq, 1);
    assert_eq!(ledger[0].kind, "job-submitted");
    assert_eq!(ledger[0].actor, "human");
    assert_eq!(ledger[0].payload["prompt"], "fix the flaky test");
    Ok(())
}

#[test]
fn ledger_rejects_update_and_delete_at_the_database_level() -> TestResult {
    let h = Harness::new()?;
    h.submit("one")?;
    let db = h.raw_db()?;

    let update = db.execute("UPDATE events SET kind = 'tampered'", []);
    let delete = db.execute("DELETE FROM events", []);

    assert!(update.is_err());
    assert!(delete.is_err());
    let ledger = h.ledger()?;
    assert_eq!(ledger[0].kind, "job-submitted");
    Ok(())
}

#[test]
fn status_lists_submitted_jobs_as_json_lines_when_piped() -> TestResult {
    let h = Harness::new()?;
    let first = h.submit("first job")?;
    let second = h.submit("second job")?;

    let cli = h.run(&["status"])?;

    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let jobs = cli.json_lines()?;
    assert_eq!(jobs.len(), 2);
    assert_eq!(jobs[0]["id"], second.as_str());
    assert_eq!(jobs[1]["id"], first.as_str());
    assert_eq!(jobs[1]["prompt"], "first job");
    Ok(())
}

#[test]
fn status_accepts_a_unique_prefix_and_at_last() -> TestResult {
    let h = Harness::new()?;
    let first = h.submit("first job")?;
    let second = h.submit("second job")?;

    let by_last = h.run(&["status", "@last"])?;
    let by_prefix = h.run(&["status", &first.to_lowercase()])?;

    assert_eq!(by_last.code, 0, "stderr: {}", by_last.stderr);
    let last = by_last.json_lines()?;
    assert_eq!(last[0]["id"], second.as_str());
    assert_eq!(last[0]["events"][0]["kind"], "job-submitted");
    assert_eq!(by_prefix.code, 0, "stderr: {}", by_prefix.stderr);
    assert_eq!(by_prefix.json_lines()?[0]["id"], first.as_str());
    Ok(())
}

#[test]
fn ambiguous_prefix_lists_matches_and_exits_3() -> TestResult {
    let h = Harness::new()?;
    let first = h.submit("first job")?;
    let second = h.submit("second job")?;

    let cli = h.run(&["status", &first[..6]])?;

    assert_eq!(cli.code, 3);
    assert!(cli.stderr.contains(&first));
    assert!(cli.stderr.contains(&second));
    Ok(())
}

#[test]
fn unknown_id_exits_3() -> TestResult {
    let h = Harness::new()?;
    h.submit("only job")?;

    let cli = h.run(&["status", "ZZZZ"])?;

    assert_eq!(cli.code, 3);
    assert!(cli.stdout.is_empty());
    Ok(())
}

#[test]
fn at_last_with_no_jobs_exits_3() -> TestResult {
    let h = Harness::new()?;

    let cli = h.run(&["status", "@last"])?;

    assert_eq!(cli.code, 3);
    Ok(())
}

#[test]
fn no_arguments_exits_2_with_usage() -> TestResult {
    let h = Harness::new()?;

    let cli = h.run(&[])?;

    assert_eq!(cli.code, 2);
    assert!(cli.stdout.contains("Exit codes"));
    Ok(())
}

#[test]
fn empty_prompt_is_a_usage_error() -> TestResult {
    let h = Harness::new()?;

    let cli = h.run(&["  "])?;

    assert_eq!(cli.code, 2);
    assert!(h.ledger().map(|l| l.is_empty()).unwrap_or(true));
    Ok(())
}

#[test]
fn additional_parameters_are_persisted_on_the_submitted_event() -> TestResult {
    let h = Harness::new()?;
    let cli = h.run(&[
        "--report-only",
        "--max-stages",
        "2",
        "--risk",
        "customer-data",
        "tidy",
    ])?;
    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let options = &h.ledger()?[0].payload["options"];
    assert_eq!(options["report_only"], true);
    assert_eq!(options["max_stages"], 2);
    assert_eq!(options["risk_flags"][0], "customer-data");
    Ok(())
}

#[test]
fn no_additional_parameters_leaves_the_event_without_an_options_key() -> TestResult {
    let h = Harness::new()?;
    h.submit("plain")?;
    assert!(h.ledger()?[0].payload.get("options").is_none());
    Ok(())
}

#[test]
fn invalid_additional_parameters_exit_with_a_usage_error() -> TestResult {
    let h = Harness::new()?;
    for args in [
        vec!["--max-stages", "0", "x"],
        vec!["--max-stages", "11", "x"],
        vec!["--context", "   ", "x"],
        vec!["--risk", "Not Kebab", "x"],
    ] {
        let cli = h.run(&args)?;
        assert_eq!(cli.code, 2, "{args:?}: {}", cli.stderr);
    }
    assert!(h.ledger()?.is_empty());
    Ok(())
}
