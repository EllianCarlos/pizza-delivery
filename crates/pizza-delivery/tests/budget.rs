mod harness;

use harness::{Harness, TestResult, VALID_HANDOFF_AGENT};

fn agent_reporting(tokens: u64) -> String {
    let plain = r#"echo '{"type":"result","is_error":false}'"#;
    let reporting = format!(
        r#"case "$PIZZA_DELIVERY_HANDOFF" in *-implement/*) echo '{{"type":"result","is_error":false,"usage":{{"input_tokens":{tokens},"output_tokens":0}}}}';; *) {plain};; esac"#
    );
    VALID_HANDOFF_AGENT.replace(plain, &reporting)
}

fn of_kind(h: &Harness, id: &str, kind: &str) -> TestResult<Vec<serde_json::Value>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id && r.kind == kind)
        .map(|r| r.payload)
        .collect())
}

fn implement_estimate_tokens(h: &Harness, id: &str) -> TestResult<serde_json::Value> {
    let estimated = of_kind(h, id, "budget-estimated")?;
    let stages = estimated
        .first()
        .and_then(|p| p["stages"].as_array())
        .ok_or("no budget-estimated event")?;
    let implement = stages
        .iter()
        .find(|s| s["stage"] == "implement")
        .ok_or("no implement estimate")?;
    Ok(implement["tokens"].clone())
}

fn has_high_budget_feedback(h: &Harness, id: &str) -> TestResult<bool> {
    Ok(of_kind(h, id, "feedback-recorded")?
        .iter()
        .any(|f| f["kind"] == "budget" && f["severity"] == "high"))
}

#[test]
fn a_cold_start_job_is_estimated_from_defaults_and_the_actual_is_recorded_beside_it() -> TestResult
{
    let h = Harness::with_agent(&agent_reporting(1234))?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    assert_eq!(implement_estimate_tokens(&h, &id)?, 1_000_000);
    let recorded = of_kind(&h, &id, "stage-budget-recorded")?;
    let payload = recorded
        .iter()
        .find(|p| p["stage"] == "implement")
        .ok_or("no recorded actuals")?;
    assert_eq!(payload["tier"], "large");
    assert_eq!(payload["estimate_tokens"], 1_000_000);
    assert_eq!(payload["actual_tokens"], 1234);
    assert!(payload["actual_wall_ms"].is_number());
    Ok(())
}

#[test]
fn a_later_job_is_estimated_from_the_median_of_recorded_actuals() -> TestResult {
    let h = Harness::with_agent(&agent_reporting(4321))?;
    let first = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&first)?;
    let second = h.submit_in_repo("do another thing")?;
    h.wait_for_end(&second)?;

    assert_eq!(implement_estimate_tokens(&h, &second)?, 4321);
    Ok(())
}

#[test]
fn an_estimate_above_the_cap_runs_with_a_high_severity_feedback_entry() -> TestResult {
    let h = Harness::with_agent(&agent_reporting(10))?
        .with_env("PIZZA_DELIVERY_BUDGET_CAP_TOKENS", "100000");
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    assert!(has_high_budget_feedback(&h, &id)?);
    Ok(())
}

#[test]
fn actual_usage_above_the_cap_parks_the_job_as_needs_human_with_feedback() -> TestResult {
    let h = Harness::with_agent(&agent_reporting(3_000_000))?
        .with_env("PIZZA_DELIVERY_BUDGET_CAP_TOKENS", "2000000");
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_state(&id, &["needs-human", "completed", "failed"])?;

    assert_eq!(job["state"], "needs-human");
    assert!(has_high_budget_feedback(&h, &id)?);
    assert!(of_kind(&h, &id, "job-completed")?.is_empty());
    Ok(())
}

#[test]
fn actual_usage_within_the_cap_does_not_park_the_job() -> TestResult {
    let h = Harness::with_agent(&agent_reporting(1_500_000))?
        .with_env("PIZZA_DELIVERY_BUDGET_CAP_TOKENS", "2000000");
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    assert!(!has_high_budget_feedback(&h, &id)?);
    Ok(())
}

#[test]
fn configured_tier_defaults_replace_the_built_in_cold_start_estimates() -> TestResult {
    let h = Harness::with_agent(&agent_reporting(10))?
        .with_env("PIZZA_DELIVERY_ESTIMATE_LARGE_TOKENS", "777")
        .with_env("PIZZA_DELIVERY_ESTIMATE_LARGE_WALL_MS", "55");
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    assert_eq!(implement_estimate_tokens(&h, &id)?, 777);
    let estimated = of_kind(&h, &id, "budget-estimated")?;
    let wall = estimated
        .first()
        .and_then(|p| p["stages"].as_array())
        .and_then(|s| s.iter().find(|s| s["stage"] == "implement"))
        .map(|s| s["wall_ms"].clone());
    assert_eq!(wall, Some(serde_json::json!(55)));
    Ok(())
}
