mod harness;

use std::time::{Duration, Instant};

use harness::{Harness, TestResult, VALID_HANDOFF_AGENT};

const QUOTA_EXHAUSTED_AGENT: &str = r#"#!/bin/sh
echo '{"type":"result","is_error":true,"terminal_reason":"api_error","api_error_status":429}'
exit 1
"#;

const CREDENTIALS_MISSING_AGENT: &str = r#"#!/bin/sh
echo "No usable credentials found for provider 'gemini'." >&2
exit 1
"#;

fn flaky_agent() -> String {
    format!(
        r#"#!/bin/sh
marker="$(dirname "$PIZZA_DELIVERY_HANDOFF")/../../ran-once"
if [ ! -e "$marker" ]; then
  touch "$marker"
  reset=$(( $(date +%s) + 2 ))
  echo '{{"type":"rate_limit_event","rate_limit_info":{{"status":"rejected","resetsAt":'"$reset"'}}}}'
  echo '{{"type":"result","is_error":true,"terminal_reason":"api_error","api_error_status":429}}'
  exit 1
fi
{}"#,
        VALID_HANDOFF_AGENT.trim_start_matches("#!/bin/sh\n")
    )
}

fn kinds(h: &Harness, id: &str) -> TestResult<Vec<String>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id)
        .map(|r| r.kind)
        .collect())
}

fn wait_for_state(h: &Harness, id: &str, wanted: &str) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let cli = h.run(&["status", id])?;
        let job = cli.json_lines()?.into_iter().next().ok_or("no status")?;
        if job["state"] == wanted {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("job never reached {wanted}: {job}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

const TWO_CLAUDES: &str = r#"
[agents.first]
adapter = "claude"
command = "{root}/first"
[agents.second]
adapter = "claude"
command = "{root}/second"

[[rules]]
chain = [{ agent = "first", model = "opus" }, { agent = "second" }]
"#;

#[test]
fn quota_exhaustion_falls_back_and_status_quota_shows_the_exhausted_agent() -> TestResult {
    let h = Harness::with_routing(
        &[
            ("first", QUOTA_EXHAUSTED_AGENT),
            ("second", VALID_HANDOFF_AGENT),
        ],
        TWO_CLAUDES,
    )?;
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let kinds = kinds(&h, &id)?;
    assert!(kinds.iter().filter(|k| *k == "agent-invoked").count() >= 2);
    assert!(kinds.contains(&"quota-signal".to_owned()));
    assert!(kinds.contains(&"agent-fell-back".to_owned()));
    let quota = h.run(&["status", "--quota", "--json"])?;
    let rows = quota.json_lines()?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["agent"], "first");
    assert_eq!(rows[0]["state"], "exhausted");
    Ok(())
}

#[test]
fn a_parked_job_waits_for_the_reset_then_resumes_and_completes_on_its_own() -> TestResult {
    let h = Harness::with_routing(
        &[("flaky", &flaky_agent())],
        r#"
[agents.flaky]
adapter = "claude"
command = "{root}/flaky"

[[rules]]
chain = [{ agent = "flaky" }]
"#,
    )?;
    let id = h.submit_in_repo("do the thing")?;

    wait_for_state(&h, &id, "waiting-for-quota")?;
    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let kinds = kinds(&h, &id)?;
    let parked = kinds.iter().position(|k| k == "job-parked");
    let succeeded = kinds.iter().position(|k| k == "stage-succeeded");
    assert!(parked.is_some() && parked < succeeded);
    assert_eq!(job["stages"][0]["attempt"], 1);
    Ok(())
}

#[test]
fn skill_driven_stages_are_never_run_by_an_adapter_without_skills() -> TestResult {
    let h = Harness::with_routing(
        &[
            ("first", CREDENTIALS_MISSING_AGENT),
            ("second", VALID_HANDOFF_AGENT),
        ],
        r#"
[agents.first]
adapter = "hermes"
command = "{root}/first"
[agents.second]
adapter = "claude"
command = "{root}/second"

[[rules]]
chain = [{ agent = "first" }, { agent = "second" }]
"#,
    )?;
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let rows: Vec<_> = h.ledger()?.into_iter().filter(|r| r.job_id == id).collect();
    let invoked: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == "agent-invoked")
        .map(|r| r.payload["agent"].clone())
        .collect();
    assert!(!invoked.is_empty());
    assert!(invoked.iter().all(|a| a == "second"), "{invoked:?}");
    assert!(!rows.iter().any(|r| r.kind == "agent-fell-back"));
    Ok(())
}

#[test]
fn an_invalid_routing_config_stops_the_daemon_before_it_serves_anything() -> TestResult {
    let h = Harness::with_routing(&[], "[[rules]]\nchain = [{ agent = \"ghost\" }]\n")?;

    let cli = h.run(&["daemon"])?;

    assert_ne!(cli.code, 0);
    assert!(cli.stderr.contains("ghost"), "stderr was: {}", cli.stderr);
    Ok(())
}
