mod harness;

use harness::{Harness, TestResult};

fn seed_event(
    h: &Harness,
    n: i64,
    job: &str,
    kind: &str,
    ts_ms: i64,
    payload: serde_json::Value,
) -> TestResult {
    h.raw_db()?.execute(
        "INSERT INTO events (id, job_id, stage_id, seq, ts_ms, actor, kind, payload)
         VALUES (?1, ?2, 's1', ?3, ?4, 'agent', ?5, ?6)",
        rusqlite::params![
            format!("SEED{n}"),
            job,
            100 + n,
            ts_ms,
            kind,
            payload.to_string()
        ],
    )?;
    Ok(())
}

fn feedback(
    h: &Harness,
    n: i64,
    job: &str,
    sev: &str,
    text: &str,
    conf: Option<u8>,
    ts: i64,
) -> TestResult {
    let mut p = serde_json::json!({"kind":"seedrisk","severity":sev,"text":text});
    if let Some(c) = conf {
        p["confidence"] = c.into();
    }
    seed_event(h, n, job, "feedback-recorded", ts, p)
}

fn items(cli: &harness::Cli) -> TestResult<Vec<serde_json::Value>> {
    let lines = cli.json_lines()?;
    assert_eq!(lines.len(), 1, "stdout: {}", cli.stdout);
    Ok(lines[0]["items"].as_array().ok_or("no items")?.clone())
}

#[test]
fn inbox_filters_searches_and_orders_newest_first() -> TestResult {
    let h = Harness::new()?;
    let a = h.submit("first")?;
    let b = h.submit("second")?;
    feedback(&h, 1, &a, "high", "Leaky cache", Some(90), 1_000)?;
    feedback(&h, 2, &b, "low", "minor nit", None, 2_000)?;
    feedback(&h, 3, &b, "high", "Bad cache key", Some(40), 3_000)?;

    let all = h.run(&["inbox", "--kind", "seedrisk", "--json"])?;
    assert_eq!(all.code, 0, "stderr: {}", all.stderr);
    let texts: Vec<_> = items(&all)?
        .iter()
        .map(|i| i["text"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(texts, ["Bad cache key", "minor nit", "Leaky cache"]);

    let high = items(&h.run(&[
        "inbox",
        "--kind",
        "seedrisk",
        "--severity",
        "high",
        "--json",
    ])?)?;
    assert_eq!(high.len(), 2);
    let conf = items(&h.run(&[
        "inbox",
        "--kind",
        "seedrisk",
        "--min-confidence",
        "50",
        "--json",
    ])?)?;
    assert_eq!(conf.len(), 1);
    let searched = items(&h.run(&[
        "inbox", "--kind", "seedrisk", "-q", "CACHE", "--job", &b, "--json",
    ])?)?;
    assert_eq!(searched.len(), 1);
    assert_eq!(searched[0]["text"], "Bad cache key");
    let limited = items(&h.run(&["inbox", "--kind", "seedrisk", "--limit", "1", "--json"])?)?;
    assert_eq!(limited.len(), 1);
    let none = items(&h.run(&["inbox", "--kind", "seedrisk", "-q", "zzz", "--json"])?)?;
    assert!(none.is_empty());
    Ok(())
}

#[test]
fn events_and_artifacts_filter_by_kind_actor_path_and_time() -> TestResult {
    let h = Harness::new()?;
    let a = h.submit("only")?;
    seed_event(
        &h,
        1,
        &a,
        "artifact-recorded",
        10,
        serde_json::json!({"kind":"diff","path":"src/auth/mod.rs","sha256":"x"}),
    )?;
    seed_event(
        &h,
        2,
        &a,
        "artifact-recorded",
        20,
        serde_json::json!({"kind":"handoff","path":"docs/h.md","sha256":"y"}),
    )?;
    seed_event(
        &h,
        3,
        &a,
        "stage-failed",
        30,
        serde_json::json!({"reason":"boom"}),
    )?;

    let arts = items(&h.run(&["artifacts", "--path", "auth", "--json"])?)?;
    assert_eq!(arts.len(), 1);
    assert_eq!(arts[0]["kind"], "diff");
    let by_kind = items(&h.run(&["artifacts", "--kind", "handoff", "-q", "DOCS", "--json"])?)?;
    assert_eq!(by_kind.len(), 1);

    let failed = items(&h.run(&[
        "events",
        "--kind",
        "stage-failed",
        "--actor",
        "agent",
        "--json",
    ])?)?;
    assert_eq!(failed.len(), 1);
    let recent = items(&h.run(&[
        "events",
        "--since",
        "1h",
        "--kind",
        "stage-failed",
        "--json",
    ])?)?;
    assert!(recent.is_empty(), "seeded events are from 1970");
    let ranged = items(&h.run(&[
        "events",
        "--since",
        "1970-01-01T00:00:00Z",
        "--until",
        "1970-01-01T00:00:01Z",
        "--kind",
        "artifact-recorded",
        "--json",
    ])?)?;
    assert_eq!(ranged.len(), 2);
    let human = h.run(&["events", "--kind", "stage-failed", "--actor", "agent"])?;
    assert_eq!(human.code, 0);
    Ok(())
}

#[test]
fn errors_use_existing_exit_codes_and_status_is_unchanged() -> TestResult {
    let h = Harness::new()?;
    let a = h.submit("only")?;
    assert_eq!(h.run(&["events", "--job", "ZZZZ"])?.code, 3);
    assert_eq!(
        h.run(&["inbox", "--kind", "seedrisk", "--since", "1h", "--until", "3h"])?
            .code,
        2
    );
    assert_eq!(h.run(&["artifacts", "--since", "garbage"])?.code, 2);
    let status = h.run(&["status"])?.json_lines()?;
    assert_eq!(status.len(), 1);
    assert_eq!(status[0]["id"], a.as_str());
    Ok(())
}
