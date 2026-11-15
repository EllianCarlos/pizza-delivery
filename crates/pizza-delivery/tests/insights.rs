mod harness;

use harness::{Harness, TestResult};

fn harness() -> TestResult<Harness> {
    let script = harness::VALID_HANDOFF_AGENT.replace(
        "## Assumptions\nnone",
        "## Assumptions\n- The API stays v1 [confidence: 70; alternatives: bump to v2; impact: clients break]",
    );
    Harness::with_agent(&script)
}

fn finished_job(h: &Harness) -> TestResult {
    let id = h.submit_in_repo("build the thing")?;
    assert_eq!(h.wait_for_end(&id)?["state"], "completed");
    Ok(())
}

fn entry_ids(h: &Harness) -> TestResult<Vec<String>> {
    let cli = h.run(&["inbox"])?;
    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    Ok(cli
        .json_lines()?
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .collect())
}

fn suppressed_by_rule(h: &Harness) -> TestResult<usize> {
    Ok(h.ledger()?
        .iter()
        .filter(|r| r.kind == "feedback-suppressed" && r.payload["decision"] == "rule-suppressed")
        .count())
}

#[test]
fn a_dismissed_entry_becomes_a_removable_rule_that_keeps_the_same_entry_out() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let ids = entry_ids(&h)?;
    assert_eq!(h.run(&["inbox", "dismiss", &ids[0]])?.code, 0);

    let rules = h.run(&["suppressions"])?.json_lines()?;
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0]["reason"], "dismissed");
    let rule = rules[0]["id"].as_str().unwrap_or_default().to_owned();

    let open_before = entry_ids(&h)?.len();
    finished_job(&h)?;
    let suppressed = suppressed_by_rule(&h)?;
    assert!(suppressed >= 1);
    assert_eq!(entry_ids(&h)?.len(), open_before);

    let removed = h.run(&["suppressions", "remove", &rule])?;
    assert_eq!(removed.code, 0, "stderr: {}", removed.stderr);
    assert!(h.run(&["suppressions"])?.json_lines()?.is_empty());

    finished_job(&h)?;
    assert_eq!(suppressed_by_rule(&h)?, suppressed);
    assert_eq!(entry_ids(&h)?.len(), open_before + suppressed);
    assert_eq!(h.run(&["suppressions", "remove", &rule])?.code, 3);
    Ok(())
}

#[test]
fn insights_report_the_ledger_as_json_lines_and_over_http() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let ids = entry_ids(&h)?;
    assert_eq!(h.run(&["inbox", "dismiss", &ids[0]])?.code, 0);

    let cli = h.run(&["insights", "--json"])?;
    assert_eq!(cli.code, 0, "stderr: {}", cli.stderr);
    let lines = cli.json_lines()?;
    let feedback = lines.iter().find(|l| l["section"] == "feedback");
    assert_eq!(feedback.map(|l| l["dismissed"].clone()), Some(1.into()));
    assert!(lines.iter().any(|l| l["section"] == "stage"));

    let api = h.get("/api/insights")?;
    assert_eq!(api["feedback"]["dismissed"], 1);
    assert_eq!(
        h.get("/api/suppressions")?.as_array().map(Vec::len),
        Some(1)
    );
    Ok(())
}
