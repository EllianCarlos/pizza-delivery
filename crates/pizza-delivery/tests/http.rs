mod harness;

use harness::{Harness, TestResult};
use serde_json::json;

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

#[test]
fn jobs_are_listed_and_grouped_by_state_over_http() -> TestResult {
    let h = harness()?;
    let id = finished_job(&h)?;

    let jobs = h.get("/api/jobs")?;
    assert_eq!(jobs["jobs"][0]["id"], id.as_str());
    assert_eq!(jobs["jobs"][0]["state"], "completed");

    let none = h.get("/api/jobs?state=running")?;
    assert_eq!(none["jobs"], json!([]));

    let board = h.get("/api/board")?;
    let completed = board["columns"]
        .as_array()
        .and_then(|cols| cols.iter().find(|c| c["state"] == "completed"))
        .ok_or("no completed column")?;
    assert_eq!(completed["jobs"][0]["id"], id.as_str());
    Ok(())
}

#[test]
fn job_detail_carries_plan_with_estimates_stages_and_feedback() -> TestResult {
    let h = harness()?;
    let id = finished_job(&h)?;

    let detail = h.get(&format!("/api/jobs/{id}"))?;
    assert_eq!(detail["job"]["state"], "completed");
    let plan = detail["plan"]["stages"].as_array().ok_or("no plan")?;
    assert!(!plan.is_empty());
    assert!(plan[0]["reason"].as_str().is_some_and(|r| !r.is_empty()));
    assert!(plan[0]["estimated_tokens"].is_number());
    assert!(!detail["stages"].as_array().ok_or("no stages")?.is_empty());
    assert_eq!(detail["feedback"][0]["kind"], "assumption");
    assert!(!detail["events"].as_array().ok_or("no events")?.is_empty());

    let (status, body) = h.http("GET", "/api/jobs/nope", None)?;
    assert_eq!(status, 404);
    assert_eq!(body["kind"], "not-found");
    Ok(())
}

#[test]
fn quota_and_inbox_are_readable() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    assert!(h.get("/api/quota")?["agents"].is_array());
    assert_eq!(h.get("/api/inbox")?["entries"][0]["status"], "open");
    Ok(())
}

#[test]
fn submit_reply_dismiss_and_cancel_work_over_http() -> TestResult {
    let h = harness()?;
    let repo = h.repo()?.to_string_lossy().into_owned();
    let (status, submitted) = h.http(
        "POST",
        "/api/jobs",
        Some(&json!({"prompt": "build the thing", "repo": repo})),
    )?;
    assert_eq!(status, 200, "{submitted}");
    let id = submitted["id"].as_str().ok_or("no id")?.to_owned();
    h.wait_for_end(&id)?;
    let other = finished_job(&h)?;
    assert_ne!(id, other);

    let entries = h.get("/api/inbox")?["entries"].clone();
    let ids: Vec<String> = entries
        .as_array()
        .ok_or("no entries")?
        .iter()
        .filter_map(|e| e["id"].as_str().map(str::to_owned))
        .collect();
    assert!(ids.len() >= 2);
    let (entry, spare) = (ids[0].clone(), ids[1].clone());
    let (status, replied) = h.http(
        "POST",
        "/api/inbox/reply",
        Some(&json!({"targets": [entry], "text": "use v2"})),
    )?;
    assert_eq!(status, 200, "{replied}");
    assert_eq!(replied["answered"][0], entry.as_str());
    let follow_up = replied["id"].as_str().ok_or("no follow-up")?.to_owned();

    let (status, cancelled) = h.http(
        "POST",
        &format!("/api/jobs/{follow_up}/cancel"),
        Some(&json!({})),
    )?;
    assert_eq!(status, 200, "{cancelled}");

    let (status, _) = h.http(
        "POST",
        "/api/inbox/dismiss",
        Some(&json!({"entries": [spare]})),
    )?;
    assert_eq!(status, 200);
    Ok(())
}

#[test]
fn bad_writes_are_rejected_and_nothing_else_is_writable() -> TestResult {
    let h = harness()?;
    let (status, body) = h.http("POST", "/api/jobs", Some(&json!({"prompt": "  "})))?;
    assert_eq!(status, 400);
    assert_eq!(body["kind"], "usage");
    for path in ["/api/stop", "/api/daemon", "/api/jobs/x/delete"] {
        let (status, _) = h.http("POST", path, Some(&json!({})))?;
        assert_eq!(status, 404, "{path}");
    }
    let (status, _) = h.http("DELETE", "/api/jobs", None)?;
    assert_eq!(status, 405);
    Ok(())
}

#[test]
fn events_resume_from_a_cursor() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let all = h.get("/api/events")?;
    let events = all["events"].as_array().ok_or("no events")?;
    assert!(events.len() > 3);
    let cursors: Vec<u64> = events.iter().filter_map(|e| e["cursor"].as_u64()).collect();
    assert!(cursors.windows(2).all(|w| w[0] < w[1]));
    let middle = cursors[cursors.len() / 2];

    let rest = h.get(&format!("/api/events?since={middle}"))?;
    let later: Vec<u64> = rest["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter_map(|e| e["cursor"].as_u64())
        .collect();
    assert_eq!(
        later,
        cursors
            .iter()
            .copied()
            .filter(|c| *c > middle)
            .collect::<Vec<_>>()
    );
    assert_eq!(rest["next"], *cursors.last().ok_or("empty")?);
    Ok(())
}

#[test]
fn stream_resumes_from_last_event_id_and_delivers_live_events() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let all = h.get("/api/events")?;
    let cursors: Vec<u64> = all["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter_map(|e| e["cursor"].as_u64())
        .collect();
    let resume = cursors[cursors.len() - 3];

    let mut sse = h.stream("/api/stream", Some(resume))?;
    let replayed: Vec<u64> = (0..2)
        .map(|_| sse.next_frame().map(|f| f.id))
        .collect::<TestResult<_>>()?;
    assert_eq!(replayed, cursors[cursors.len() - 2..]);

    let id = h.submit("a brand new job")?;
    loop {
        let frame = sse.next_frame()?;
        assert!(frame.id > resume);
        if frame.event == "job-submitted" && frame.data["job_id"] == id.as_str() {
            assert_eq!(frame.data["cursor"], frame.id);
            break;
        }
    }
    Ok(())
}
