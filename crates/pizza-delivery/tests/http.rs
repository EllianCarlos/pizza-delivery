mod harness;

use harness::{Harness, TestResult};
use serde_json::json;

fn harness() -> TestResult<Harness> {
    let script = harness::VALID_HANDOFF_AGENT.replace(
        "## Assumptions\nnone",
        "## Assumptions\n- The API stays v1 [confidence: 70; alternatives: bump to v2; impact: clients break; evidence: Cargo.toml]",
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
fn job_detail_endpoint_bundles_job_stages_events_and_artifacts() -> TestResult {
    let h = harness()?;
    let id = finished_job(&h)?;

    let plain = h.get(&format!("/api/jobs/{id}"))?;
    let full = h.get(&format!("/api/jobs/{id}/detail"))?;
    for key in ["job", "plan", "stages", "feedback", "events"] {
        assert_eq!(full[key], plain[key], "{key} matches the plain detail");
    }
    let recorded = full["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter(|e| e["kind"] == "artifact-recorded")
        .count();
    let artifacts = full["artifacts"].as_array().ok_or("no artifacts")?;
    assert_eq!(artifacts.len(), recorded);
    for (n, a) in artifacts.iter().enumerate() {
        assert_eq!(a["index"], n);
        assert!(a["path"].is_string() && a["kind"].is_string() && a["sha256"].is_string());
        let content = h.get(&format!("/api/jobs/{id}/artifacts/{n}"))?;
        assert_eq!(content["path"], a["path"]);
    }

    let (status, body) = h.http("GET", "/api/jobs/nope/detail", None)?;
    assert_eq!(status, 404);
    assert_eq!(body["kind"], "not-found");
    let (status, _) = h.http("POST", &format!("/api/jobs/{id}/detail"), Some(&json!({})))?;
    assert_eq!(status, 405);
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
fn inbox_entries_carry_their_evidence() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    assert_eq!(h.get("/api/inbox")?["entries"][0]["evidence"], "Cargo.toml");
    Ok(())
}

#[test]
fn inbox_is_filtered_and_searched_by_query_parameters() -> TestResult {
    let h = harness()?;
    finished_job(&h)?;
    let count = |path: &str| -> TestResult<usize> {
        Ok(h.get(path)?["entries"]
            .as_array()
            .ok_or("no entries")?
            .len())
    };
    let all = count("/api/inbox")?;
    assert!(all >= 1);
    assert_eq!(count("/api/inbox?q=nothing-matches-this")?, 0);
    assert_eq!(count("/api/inbox?q=api%20stays")?, all);
    assert_eq!(count("/api/inbox?q=API+STAYS")?, all);
    assert_eq!(count("/api/inbox?status=open")?, all);
    assert_eq!(count("/api/inbox?status=dismissed")?, 0);
    assert_eq!(count("/api/inbox?status=dismissed,open")?, all);
    assert_eq!(count("/api/inbox?status=open&q=nothing-matches-this")?, 0);
    assert_eq!(count("/api/inbox?limit=1")?, 1);
    Ok(())
}

#[test]
fn dry_run_returns_the_triage_and_enqueues_nothing() -> TestResult {
    let h = harness()?;
    let repo = h.repo()?.to_string_lossy().into_owned();
    let (status, out) = h.http(
        "POST",
        "/api/dry-run",
        Some(&json!({
            "prompt": "add a toggle",
            "repo": repo,
            "options": {"template": "bug-fix", "report_only": true}
        })),
    )?;
    assert_eq!(status, 200, "{out}");
    assert_eq!(out["outcome"]["triage"]["template"], "bug-fix");
    assert!(out["outcome"]["triage"]["plan"].is_array());
    assert!(out["context_sources"].is_array());
    assert_eq!(h.get("/api/jobs")?["jobs"], json!([]));
    assert!(h.ledger().map(|l| l.is_empty()).unwrap_or(true));

    let (status, body) = h.http("POST", "/api/dry-run", Some(&json!({"prompt": "  "})))?;
    assert_eq!(status, 400);
    assert_eq!(body["kind"], "usage");
    let (status, _) = h.http(
        "POST",
        "/api/dry-run",
        Some(&json!({"prompt": "x", "options": {"max_stages": 0}})),
    )?;
    assert_eq!(status, 400);
    let (status, _) = h.http("GET", "/api/dry-run", None)?;
    assert_eq!(status, 405);
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
fn submit_accepts_and_validates_additional_parameters() -> TestResult {
    let h = harness()?;
    let repo = h.repo()?.to_string_lossy().into_owned();
    let (status, ok) = h.http(
        "POST",
        "/api/jobs",
        Some(&json!({
            "prompt": "fix the build",
            "repo": repo,
            "options": {"template": "bug-fix", "max_stages": 4, "context_hint": "auth", "risk_flags": ["production"], "model": "haiku"},
        })),
    )?;
    assert_eq!(status, 200, "{ok}");
    for bad in [
        json!({"max_stages": 0}),
        json!({"context_hint": "  "}),
        json!({"risk_flags": ["Not Kebab"]}),
        json!({"model": "claude sonnet"}),
    ] {
        let (status, body) = h.http(
            "POST",
            "/api/jobs",
            Some(&json!({"prompt": "x", "options": bad})),
        )?;
        assert_eq!(status, 400, "{bad}: {body}");
        assert_eq!(body["kind"], "usage");
    }
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
fn artifact_content_is_readable_by_its_position_in_the_job() -> TestResult {
    let h = harness()?;
    let id = finished_job(&h)?;

    let detail = h.get(&format!("/api/jobs/{id}"))?;
    let recorded: Vec<&serde_json::Value> = detail["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter(|e| e["kind"] == "artifact-recorded")
        .collect();
    assert!(!recorded.is_empty());
    let path = recorded[0]["payload"]["path"].as_str().ok_or("no path")?;

    let artifact = h.get(&format!("/api/jobs/{id}/artifacts/0"))?;
    assert_eq!(artifact["path"], path);
    let on_disk = std::fs::read_to_string(h.data_dir().join(path))?;
    assert_eq!(artifact["content"], on_disk.as_str());

    let (status, body) = h.http("GET", &format!("/api/jobs/{id}/artifacts/999"), None)?;
    assert_eq!(status, 404);
    assert_eq!(body["kind"], "not-found");
    Ok(())
}

#[test]
fn the_web_ui_is_served_from_the_root() -> TestResult {
    let h = harness()?;
    let (status, head, body) = h.raw_get("/")?;
    assert_eq!(status, 200);
    assert!(head.contains("Content-Type: text/html"), "{head}");
    assert!(body.contains("<div id=\"root\">"), "{body}");
    let (status, head, _) = h.raw_get(&script_path(&body)?)?;
    assert_eq!(status, 200);
    assert!(head.contains("javascript"), "{head}");
    Ok(())
}

fn script_path(index: &str) -> TestResult<String> {
    let rest = index
        .split("<script type=\"module\" src=\"")
        .nth(1)
        .ok_or("index.html has no module script")?;
    Ok(rest.split('"').next().unwrap_or_default().to_owned())
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.trim())
}

#[test]
fn the_shell_revalidates_and_hashed_assets_are_immutable_and_compressed() -> TestResult {
    let h = harness()?;
    let (_, head, index) = h.raw_get("/")?;
    assert_eq!(header(&head, "cache-control"), Some("no-cache"), "{head}");
    assert_eq!(header(&head, "vary"), Some("Accept-Encoding"), "{head}");
    let (_, head, _) = h.raw_get("/sw.js")?;
    assert_eq!(header(&head, "cache-control"), Some("no-cache"), "{head}");

    let script = script_path(&index)?;
    let (_, head, plain) = h.raw_get(&script)?;
    assert_eq!(header(&head, "content-encoding"), None, "{head}");
    assert_eq!(header(&head, "vary"), Some("Accept-Encoding"), "{head}");
    let hashed = script.starts_with("/app-");
    let expected = if hashed {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    assert_eq!(header(&head, "cache-control"), Some(expected), "{head}");

    if hashed {
        let (_, head, _) = h.raw_get_with(&script, &[("Accept-Encoding", "gzip, br")])?;
        assert_eq!(header(&head, "content-encoding"), Some("br"), "{head}");
        let (_, head, _) = h.raw_get_with(&script, &[("Accept-Encoding", "gzip")])?;
        assert_eq!(header(&head, "content-encoding"), Some("gzip"), "{head}");
        let length: usize = header(&head, "content-length")
            .ok_or("no length")?
            .parse()?;
        assert!(length < plain.len(), "{length} vs {}", plain.len());
    }
    Ok(())
}

#[test]
fn the_pwa_shell_manifest_service_worker_and_job_links_are_served() -> TestResult {
    let h = harness()?;
    let (status, head, body) = h.raw_get("/manifest.webmanifest")?;
    assert_eq!(status, 200);
    assert!(head.contains("manifest+json"), "{head}");
    let manifest: serde_json::Value = serde_json::from_str(&body)?;
    assert_eq!(manifest["display"], "standalone");
    assert_eq!(manifest["start_url"], "/");

    let (status, head, body) = h.raw_get("/sw.js")?;
    assert_eq!(status, 200);
    assert!(head.contains("javascript"), "{head}");
    assert!(body.contains("addEventListener(\"push\""), "{body}");
    assert!(
        !body.contains("/api/jobs"),
        "service worker must not cache api data"
    );

    for icon in ["/icon-192.png", "/icon-512.png"] {
        let (status, head, _) = h.raw_get(icon)?;
        assert_eq!(status, 200);
        assert!(head.contains("image/png"), "{head}");
    }

    let (status, head, body) = h.raw_get("/jobs/01J9ABCDEF")?;
    assert_eq!(status, 200);
    assert!(head.contains("text/html"), "{head}");
    assert!(body.contains("<div id=\"root\">"), "{body}");
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
