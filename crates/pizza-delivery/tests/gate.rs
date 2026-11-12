mod harness;

use harness::{Harness, TestResult, VALID_HANDOFF_AGENT};

fn agent_with(assumptions: &str, questions: &str) -> String {
    VALID_HANDOFF_AGENT
        .replace(
            "## Assumptions\nnone",
            &format!("## Assumptions\n{assumptions}"),
        )
        .replace(
            "## Open questions\nnone",
            &format!("## Open questions\n{questions}"),
        )
}

fn of_kind(h: &Harness, id: &str, kind: &str) -> TestResult<Vec<serde_json::Value>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id && r.kind == kind)
        .map(|r| r.payload)
        .collect())
}

fn handoff_feedback(h: &Harness, id: &str) -> TestResult<Vec<serde_json::Value>> {
    Ok(of_kind(h, id, "feedback-recorded")?
        .into_iter()
        .filter(|p| p["kind"] != "budget")
        .collect())
}

#[test]
fn mixed_confidence_items_yield_one_inbox_entry_and_two_suppressed_ones() -> TestResult {
    let agent = agent_with(
        "- low and risky [confidence: 20; alternatives: a, b; impact: high; evidence: README]\n- middling [confidence: 60; impact: high]\n- sure [confidence: 90; impact: low]",
        "none",
    );
    let h = Harness::with_agent(&agent)?;
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    // Every pipeline stage reads the same fixture handoff, so each records its own copy.
    let recorded = handoff_feedback(&h, &id)?;
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|r| r["text"] == "low and risky"));
    let recorded_count = recorded.len();
    let recorded = &recorded;
    assert_eq!(recorded[0]["text"], "low and risky");
    assert_eq!(recorded[0]["kind"], "assumption");
    assert_eq!(recorded[0]["impact"], "high");
    assert_eq!(recorded[0]["alternatives"], serde_json::json!(["a", "b"]));
    assert_eq!(recorded[0]["evidence"], "README");
    let suppressed = of_kind(&h, &id, "feedback-suppressed")?;
    assert_eq!(suppressed.len(), 2 * recorded_count);
    let decisions: Vec<_> = suppressed
        .iter()
        .take(2)
        .map(|p| p["decision"].clone())
        .collect();
    assert_eq!(decisions, vec!["handoff-only", "auto-resolved"]);
    Ok(())
}

#[test]
fn an_item_without_metadata_reaches_the_inbox() -> TestResult {
    let h = Harness::with_agent(&agent_with("none", "- which database should we use?"))?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let recorded = handoff_feedback(&h, &id)?;
    assert!(!recorded.is_empty());
    assert!(recorded.iter().all(|r| r["kind"] == "question"));
    assert!(of_kind(&h, &id, "feedback-suppressed")?.is_empty());
    Ok(())
}

#[test]
fn configured_thresholds_move_what_reaches_the_inbox() -> TestResult {
    let agent = agent_with("- risky [confidence: 60; impact: high]", "none");
    let h = Harness::with_agent(&agent)?.with_env("PIZZA_DELIVERY_GATE_INBOX_BELOW", "70");
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    assert!(!handoff_feedback(&h, &id)?.is_empty());
    Ok(())
}

#[test]
fn a_stage_whose_handoff_has_no_items_records_no_feedback() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    assert!(handoff_feedback(&h, &id)?.is_empty());
    assert!(of_kind(&h, &id, "feedback-suppressed")?.is_empty());
    Ok(())
}

#[test]
fn the_prompt_template_is_recorded_as_version_two() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let invoked = of_kind(&h, &id, "agent-invoked")?;
    assert!(!invoked.is_empty());
    assert!(invoked.iter().all(|p| p["version"] == 2));
    Ok(())
}

#[test]
fn hard_stops_bypass_the_gate_whatever_the_thresholds() -> TestResult {
    // Everything auto-resolves at confidence 0, yet the budget cap still surfaces.
    let agent = VALID_HANDOFF_AGENT.replace(
        r#"{"type":"result","is_error":false}"#,
        r#"{"type":"result","is_error":false,"usage":{"input_tokens":3000000,"output_tokens":0}}"#,
    );
    let h = Harness::with_agent(&agent)?
        .with_env("PIZZA_DELIVERY_BUDGET_CAP_TOKENS", "2000000")
        .with_env("PIZZA_DELIVERY_GATE_INBOX_BELOW", "0")
        .with_env("PIZZA_DELIVERY_GATE_AUTO_AT", "0");
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_state(&id, &["needs-human", "completed", "failed"])?;

    assert_eq!(job["state"], "needs-human");
    assert!(of_kind(&h, &id, "feedback-recorded")?
        .iter()
        .any(|f| f["kind"] == "budget" && f["severity"] == "high"));
    Ok(())
}

#[test]
fn an_unresolved_repo_still_reaches_the_inbox_with_permissive_thresholds() -> TestResult {
    let h = Harness::new()?
        .with_env("PIZZA_DELIVERY_GATE_INBOX_BELOW", "0")
        .with_env("PIZZA_DELIVERY_GATE_AUTO_AT", "0");
    h.repo()?;
    let id = h.submit("do something unrelated")?;

    h.wait_for_state(&id, &["needs-human"])?;

    assert!(of_kind(&h, &id, "feedback-recorded")?
        .iter()
        .any(|f| f["kind"] == "needs-human"));
    Ok(())
}
