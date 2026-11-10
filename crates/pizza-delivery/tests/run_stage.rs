mod harness;

use harness::{Harness, TestResult, INVALID_HANDOFF_AGENT, VALID_HANDOFF_AGENT};
use sha2::{Digest, Sha256};

const CLAUDE_FIXTURE: &str = include_str!("../../../docs/fixtures/claude/normal.stdout.jsonl");

fn kinds(h: &Harness, id: &str) -> TestResult<Vec<String>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id)
        .map(|r| r.kind)
        .collect())
}

#[test]
fn a_stage_runs_to_success_with_a_valid_handoff() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let stages = job["stages"].as_array().ok_or("no stages")?;
    let names: Vec<_> = stages.iter().filter_map(|s| s["name"].as_str()).collect();
    assert_eq!(names, vec!["grill", "spec", "implement", "review"]);
    assert!(stages
        .iter()
        .all(|s| s["state"] == "succeeded" && s["attempt"] == 1));
    Ok(())
}

#[test]
fn the_job_worktree_is_a_pizza_branch_cut_from_the_default_branch() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let worktree = h.data_dir().join("worktrees").join("my-repo").join(&id);

    assert_eq!(
        h.vcs(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        format!("pizza/{id}")
    );
    assert_eq!(
        h.vcs(&worktree, &["merge-base", "HEAD", "main"])?,
        h.vcs(&h.repo()?, &["rev-parse", "main"])?
    );
    Ok(())
}

#[test]
fn a_finished_job_is_pushed_and_its_draft_pr_url_is_ledgered() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    let job = h.wait_for_end(&id)?;
    assert_eq!(job["state"], "completed");

    let remote = h.root().join("remotes").join("my-repo.git");
    let branches = h.vcs(&remote, &["branch", "--list", &format!("pizza/{id}")])?;
    assert!(branches.contains(&format!("pizza/{id}")));

    let shipped = h
        .ledger()?
        .into_iter()
        .filter(|r| r.job_id == id && r.kind == "sandbox-audit")
        .find(|r| r.payload["pushed_ref"].is_string())
        .ok_or("no ship audit")?;
    assert_eq!(shipped.payload["pushed_ref"], format!("origin/pizza/{id}"));
    assert_eq!(shipped.payload["pr_url"], "https://github.com/o/r/pull/1");
    assert_eq!(shipped.payload["diff_policy_result"], "clean");
    Ok(())
}

#[test]
fn the_agent_runs_in_the_worktree_as_the_leader_of_its_own_process_group() -> TestResult {
    let agent = format!(
        "#!/bin/sh\npwd > \"$(dirname \"$PIZZA_DELIVERY_HANDOFF\")/cwd.txt\"\nawk '{{print $1, $5}}' /proc/$$/stat > \"$(dirname \"$PIZZA_DELIVERY_HANDOFF\")/pgrp.txt\"\n{}",
        VALID_HANDOFF_AGENT.trim_start_matches("#!/bin/sh\n")
    );
    let h = Harness::with_agent(&agent)?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let stage_dir = h
        .data_dir()
        .join("jobs")
        .join(&id)
        .join("stages")
        .join("3-implement");
    let cwd = std::fs::read_to_string(stage_dir.join("cwd.txt"))?;
    let pgrp = std::fs::read_to_string(stage_dir.join("pgrp.txt"))?;
    let mut parts = pgrp.split_whitespace();

    assert!(cwd.trim().ends_with(&format!("worktrees/my-repo/{id}")));
    assert_eq!(parts.next(), parts.next());
    Ok(())
}

#[test]
fn artifacts_are_indexed_in_the_ledger_with_hashes_of_the_files_on_disk() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let artifacts: Vec<_> = h
        .ledger()?
        .into_iter()
        .filter(|r| r.kind == "artifact-recorded")
        .collect();

    let kinds: Vec<_> = artifacts
        .iter()
        .map(|r| r.payload["kind"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        kinds,
        vec![
            "context",
            "raw-output",
            "handoff",
            "grill-transcript",
            "raw-output",
            "handoff",
            "spec",
            "ticket",
            "raw-output",
            "handoff",
            "raw-output",
            "handoff",
            "review-report",
            "pr-body"
        ]
    );
    for row in artifacts {
        let path = row.payload["path"].as_str().ok_or("no path")?;
        let bytes = std::fs::read(h.data_dir().join(path))?;
        assert_eq!(
            row.payload["sha256"].as_str(),
            Some(format!("{:x}", Sha256::digest(&bytes)).as_str())
        );
    }
    Ok(())
}

#[test]
fn claim_and_release_of_the_lease_are_ledgered() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let ledger = kinds(&h, &id)?;

    let claimed = ledger.iter().position(|k| k == "stage-claimed");
    let released = ledger.iter().position(|k| k == "lease-released");
    assert!(claimed.is_some() && released.is_some() && claimed < released);
    Ok(())
}

#[test]
fn an_invalid_handoff_is_retried_and_then_fails_the_job() -> TestResult {
    let h = Harness::with_agent(INVALID_HANDOFF_AGENT)?;
    let id = h.submit_in_repo("do the thing")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "failed");
    let ledger = kinds(&h, &id)?;
    assert_eq!(ledger.iter().filter(|k| *k == "stage-retried").count(), 1);
    assert_eq!(ledger.iter().filter(|k| *k == "stage-failed").count(), 1);
    assert_eq!(job["stages"][0]["attempt"], 2);
    Ok(())
}

#[test]
fn unknown_usage_is_left_empty_and_known_usage_is_normalized() -> TestResult {
    let bare = Harness::new()?;
    let bare_id = bare.submit_in_repo("do the thing")?;
    bare.wait_for_end(&bare_id)?;
    let rich_agent = format!(
        "{}cat <<'FIXTURE'\n{}FIXTURE\n",
        VALID_HANDOFF_AGENT
            .trim_end()
            .trim_end_matches("echo '{\"type\":\"result\",\"is_error\":false}'"),
        CLAUDE_FIXTURE
    );
    let rich = Harness::with_agent(&rich_agent)?;
    let rich_id = rich.submit_in_repo("do the thing")?;
    rich.wait_for_end(&rich_id)?;

    let usage = |h: &Harness| -> TestResult<serde_json::Value> {
        h.ledger()?
            .into_iter()
            .find(|r| r.kind == "agent-finished")
            .map(|r| r.payload["usage"].clone())
            .ok_or_else(|| "no agent-finished".into())
    };
    let bare_usage = usage(&bare)?;
    let rich_usage = usage(&rich)?;

    assert!(bare_usage["input_tokens"].is_null());
    assert!(bare_usage["output_tokens"].is_null());
    assert!(bare_usage["cost_microusd"].is_null());
    assert!(bare_usage["model"].is_null());
    assert!(bare_usage["wall_ms"].is_u64());
    assert_eq!(rich_usage["model"], "claude-sonnet-5-5");
    assert_eq!(rich_usage["input_tokens"], 101_196);
    assert_eq!(rich_usage["output_tokens"], 8);
    assert_eq!(rich_usage["cost_microusd"], 301_268);
    Ok(())
}

#[test]
fn status_for_a_job_shows_the_stage_timeline_with_usage() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("do the thing")?;
    h.wait_for_end(&id)?;

    let cli = h.run(&["status", &id])?;

    let job = cli.json_lines()?.into_iter().next().ok_or("no output")?;
    assert_eq!(
        job["stages"][0]["usage"]["input_tokens"],
        serde_json::Value::Null
    );
    assert!(job["stages"][0]["usage"]["wall_ms"].is_u64());
    let events: Vec<_> = job["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert!(events.contains(&"stage-succeeded"));
    Ok(())
}
