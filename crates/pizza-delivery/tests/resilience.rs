mod harness;

use std::fs;
use std::process::Command;
use std::time::{Duration, Instant};

use harness::{Harness, TestResult, VALID_HANDOFF_AGENT};

fn valid_body() -> &'static str {
    VALID_HANDOFF_AGENT.trim_start_matches("#!/bin/sh\n")
}

fn wait_until(what: &str, secs: u64, mut ready: impl FnMut() -> TestResult<bool>) -> TestResult {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if ready()? {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {what}").into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn kinds(h: &Harness, id: &str) -> TestResult<Vec<String>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id)
        .map(|r| r.kind)
        .collect())
}

fn alive(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

fn worktree(h: &Harness, id: &str) -> std::path::PathBuf {
    h.data_dir().join("worktrees").join("my-repo").join(id)
}

#[test]
fn a_timeout_kills_the_whole_process_group_then_retries_once() -> TestResult {
    let script = r#"#!/bin/sh
trap '' TERM
dir=$(dirname "$PIZZA_DELIVERY_HANDOFF")
sleep 300 &
echo "$$" >> "$dir/pids.txt"
echo "$!" >> "$dir/pids.txt"
wait
"#;
    let h = Harness::with_agent(script)?
        .with_env("PIZZA_DELIVERY_STAGE_TIMEOUT_SECS", "1")
        .with_env("PIZZA_DELIVERY_KILL_GRACE_MS", "200");
    let id = h.submit_in_repo("hang")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "failed");
    let rows: Vec<_> = h.ledger()?.into_iter().filter(|r| r.job_id == id).collect();
    let retries: Vec<_> = rows.iter().filter(|r| r.kind == "stage-retried").collect();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].payload["cause"], "timeout");
    assert_eq!(rows.iter().filter(|r| r.kind == "stage-claimed").count(), 2);
    let pids = fs::read_to_string(
        h.data_dir()
            .join("jobs")
            .join(&id)
            .join("stages/1-grill/pids.txt"),
    )?;
    assert_eq!(pids.split_whitespace().count(), 4);
    for pid in pids.split_whitespace() {
        assert!(!alive(pid), "process {pid} survived the timeout");
    }
    Ok(())
}

#[test]
fn a_crash_is_recovered_as_a_new_attempt_with_a_note_and_without_duplicate_work() -> TestResult {
    let script = format!(
        "#!/bin/sh\nif [ -f wip.txt ]; then\n{}else\necho partial > wip.txt\nsleep 120\nfi\n",
        valid_body()
    );
    let h = Harness::with_agent(&script)?
        .with_env("PIZZA_DELIVERY_LEASE_TTL_MS", "600")
        .with_env("PIZZA_DELIVERY_KILL_GRACE_MS", "200")
        .with_env("PIZZA_DELIVERY_SHUTDOWN_GRACE_MS", "200");
    let id = h.submit_in_repo("crash me")?;
    wait_until("the agent to start", 15, || {
        Ok(worktree(&h, &id).join("wip.txt").exists())
    })?;
    let holder: String = h
        .raw_db()?
        .query_row("SELECT holder FROM leases", [], |r| r.get(0))?;
    let pid = holder.trim_start_matches("daemon-");
    assert!(Command::new("kill").args(["-9", pid]).status()?.success());
    // `kill` returns before the kernel has finished tearing the daemon down,
    // and a child forked by the daemon but not yet exec'd can briefly keep its
    // listening socket open. A client that connects in that window is reset
    // mid-request ("Connection reset by peer") instead of being refused and
    // starting a fresh daemon. Wait until the socket really refuses.
    let socket = h.root().join("pd.sock");
    wait_until("the killed daemon to stop listening", 15, || {
        Ok(!alive(pid) && std::os::unix::net::UnixStream::connect(&socket).is_err())
    })?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let rows: Vec<_> = h.ledger()?.into_iter().filter(|r| r.job_id == id).collect();
    let attempts: Vec<_> = rows
        .iter()
        .filter(|r| r.kind == "stage-claimed")
        .map(|r| r.payload["attempt"].clone())
        .collect();
    assert_eq!(attempts, vec![1, 2, 1, 1]);
    assert_eq!(rows.iter().filter(|r| r.kind == "job-completed").count(), 1);
    assert!(rows.iter().any(|r| r.kind == "lease-expired"));
    let retried = rows.iter().find(|r| r.kind == "stage-retried");
    assert_eq!(
        retried.map(|r| r.payload["cause"].clone()),
        Some("crash".into())
    );
    let note = fs::read_to_string(
        h.data_dir()
            .join("jobs")
            .join(&id)
            .join("stages/1-grill/interruption-1.md"),
    )?;
    assert!(note.contains("crash"));
    let log = h.vcs(&worktree(&h, &id), &["log", "--format=%s"])?;
    assert!(log.contains("wip: grill attempt 1 interrupted (crash)"));
    Ok(())
}

fn concurrency_log(h: &Harness) -> TestResult<Vec<String>> {
    let path = h.data_dir().join("concurrency.log");
    Ok(fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect())
}

fn logging_agent() -> String {
    format!(
        "#!/bin/sh\nlog=\"$(dirname \"$PIZZA_DELIVERY_HANDOFF\")/../../../../concurrency.log\"\nid=$(basename \"$(dirname \"$(dirname \"$(dirname \"$PIZZA_DELIVERY_HANDOFF\")\")\")\")\necho \"start $id\" >> \"$log\"\nsleep 0.6\necho \"end $id\" >> \"$log\"\n{}",
        valid_body()
    )
}

fn assert_never_overlapping(log: &[String]) {
    let kinds: Vec<&str> = log
        .iter()
        .map(|l| l.split_whitespace().next().unwrap_or(""))
        .collect();
    assert!(!kinds.is_empty());
    assert!(kinds.chunks(2).all(|pair| pair == ["start", "end"]));
}

#[test]
fn the_global_cap_runs_stages_one_at_a_time() -> TestResult {
    let h = Harness::with_agent(&logging_agent())?
        .with_env("PIZZA_DELIVERY_MAX_RUNNING", "1")
        .with_env("PIZZA_DELIVERY_MAX_PER_REPO", "9");
    let first = h.submit_in_repo("one")?;
    let second = h.submit_in_repo("two")?;
    h.wait_for_end(&first)?;
    h.wait_for_end(&second)?;

    assert_never_overlapping(&concurrency_log(&h)?);
    Ok(())
}

#[test]
fn the_per_repo_cap_runs_stages_of_one_repo_one_at_a_time() -> TestResult {
    let h = Harness::with_agent(&logging_agent())?
        .with_env("PIZZA_DELIVERY_MAX_RUNNING", "9")
        .with_env("PIZZA_DELIVERY_MAX_PER_REPO", "1");
    let first = h.submit_in_repo("one")?;
    let second = h.submit_in_repo("two")?;
    h.wait_for_end(&first)?;
    h.wait_for_end(&second)?;

    assert_never_overlapping(&concurrency_log(&h)?);
    Ok(())
}

#[test]
fn with_room_under_the_caps_stages_run_side_by_side() -> TestResult {
    let h = Harness::with_agent(&logging_agent())?
        .with_env("PIZZA_DELIVERY_MAX_RUNNING", "2")
        .with_env("PIZZA_DELIVERY_MAX_PER_REPO", "2");
    let first = h.submit_in_repo("one")?;
    let second = h.submit_in_repo("two")?;
    h.wait_for_end(&first)?;
    h.wait_for_end(&second)?;

    let kinds: Vec<String> = concurrency_log(&h)?
        .iter()
        .map(|l| l.split_whitespace().next().unwrap_or("").to_owned())
        .collect();
    assert_eq!(kinds[..2], ["start", "start"]);
    Ok(())
}

#[test]
fn queued_jobs_run_by_priority_then_submit_time() -> TestResult {
    let h = Harness::with_agent(&logging_agent())?.with_env("PIZZA_DELIVERY_MAX_RUNNING", "1");
    let repo = h.repo()?.to_string_lossy().into_owned();
    let id_of = |cli: harness::Cli| -> TestResult<String> {
        let lines = cli.json_lines()?;
        Ok(lines
            .first()
            .and_then(|v| v["id"].as_str())
            .ok_or("no id")?
            .to_owned())
    };
    let blocker = id_of(h.run(&["--repo", &repo, "blocker"])?)?;
    wait_until("the blocker to start", 15, || {
        Ok(!concurrency_log(&h)?.is_empty())
    })?;
    let normal = id_of(h.run(&["--repo", &repo, "normal"])?)?;
    let urgent = id_of(h.run(&["--repo", &repo, "--priority", "high", "urgent"])?)?;
    for id in [&blocker, &normal, &urgent] {
        h.wait_for_end(id)?;
    }

    let started: Vec<String> = concurrency_log(&h)?
        .into_iter()
        .filter_map(|l| l.strip_prefix("start ").map(str::to_owned))
        .collect();
    let mut order: Vec<String> = Vec::new();
    for id in started {
        if !order.contains(&id) {
            order.push(id);
        }
    }
    assert_eq!(order, vec![blocker, urgent, normal]);
    Ok(())
}

#[test]
fn cancel_terminates_the_running_stage_and_keeps_the_worktree() -> TestResult {
    let script = "#!/bin/sh\necho \"$$\" > pid.txt\nsleep 120\n";
    let h = Harness::with_agent(script)?.with_env("PIZZA_DELIVERY_KILL_GRACE_MS", "200");
    let id = h.submit_in_repo("long job")?;
    wait_until("the agent to start", 15, || {
        Ok(worktree(&h, &id).join("pid.txt").exists())
    })?;
    let pid = fs::read_to_string(worktree(&h, &id).join("pid.txt"))?
        .trim()
        .to_owned();

    let cancel = h.run(&["cancel", &id])?;
    assert_eq!(cancel.code, 0, "{}", cancel.stderr);
    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "cancelled");
    wait_until("the stage to be released", 15, || {
        Ok(kinds(&h, &id)?.contains(&"lease-released".to_owned()))
    })?;
    let seen = kinds(&h, &id)?;
    assert!(seen.contains(&"job-cancelled".to_owned()));
    assert!(!seen.contains(&"job-failed".to_owned()));
    assert!(!seen.contains(&"stage-retried".to_owned()));
    assert!(worktree(&h, &id).join(".git").exists());
    wait_until("the agent to die", 10, || Ok(!alive(&pid)))?;
    Ok(())
}

#[test]
fn cancelling_a_finished_job_is_a_usage_error() -> TestResult {
    let h = Harness::new()?;
    let id = h.submit_in_repo("quick")?;
    h.wait_for_end(&id)?;

    assert_eq!(h.run(&["cancel", &id])?.code, 2);
    Ok(())
}

#[test]
fn shutdown_lets_the_running_stage_finish_and_claims_nothing_new() -> TestResult {
    let script = format!(
        "#!/bin/sh\necho started > started.txt\nsleep 1\n{}",
        valid_body()
    );
    let h = Harness::with_agent(&script)?.with_env("PIZZA_DELIVERY_MAX_RUNNING", "1");
    let first = h.submit_in_repo("running")?;
    let second = h.submit_in_repo("waiting")?;
    wait_until("the agent to start", 15, || {
        Ok(worktree(&h, &first).join("started.txt").exists())
    })?;

    assert_eq!(h.run(&["stop"])?.code, 0);

    wait_until("the running stage to finish", 20, || {
        Ok(kinds(&h, &first)?.contains(&"stage-succeeded".to_owned()))
    })?;
    std::thread::sleep(Duration::from_millis(500));
    assert!(!kinds(&h, &second)?.contains(&"stage-claimed".to_owned()));
    let claims = kinds(&h, &first)?
        .iter()
        .filter(|k| *k == "stage-claimed")
        .count();
    assert_eq!(claims, 1);
    Ok(())
}
