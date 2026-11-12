mod harness;

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

use harness::{Harness, TestResult};

fn free_port() -> TestResult<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// Sends one raw request; returns (status, body).
fn http(
    port: u16,
    method: &str,
    path: &str,
    headers: &[&str],
    body: &str,
) -> TestResult<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut req = format!("{method} {path} HTTP/1.1\r\n");
    for h in headers {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    s.write_all(req.as_bytes())?;
    let mut raw = String::new();
    s.read_to_string(&mut raw)?;
    let status = raw.split(' ').nth(1).ok_or("no status")?.parse()?;
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    Ok((status, body))
}

fn post(port: u16, path: &str, body: &str) -> TestResult<(u16, String)> {
    let host = format!("Host: 127.0.0.1:{port}");
    http(
        port,
        "POST",
        path,
        &[&host, "Content-Type: application/json"],
        body,
    )
}

fn start(h: &Harness, port: u16) -> TestResult {
    let repo = h.repo()?;
    let cli = h.run(&[
        "--dry-run",
        "--json",
        "--repo",
        &repo.to_string_lossy(),
        "warm up",
    ])?;
    assert_eq!(cli.code, 0, "{}", cli.stderr);
    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).is_err() {
        assert!(Instant::now() < deadline, "http listener never came up");
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}

#[test]
fn the_form_submits_and_previews_through_the_daemon_and_unsafe_requests_change_nothing(
) -> TestResult {
    let port = free_port()?;
    let h = Harness::new()?.with_env("PIZZA_DELIVERY_HTTP_LISTEN", &format!("127.0.0.1:{port}"));
    start(&h, port)?;

    let host = format!("Host: 127.0.0.1:{port}");
    let (status, page) = http(port, "GET", "/", &[&host], "")?;
    assert_eq!(status, 200);
    assert!(page.contains("Additional Parameters"));

    // Dry run reflects options and enqueues nothing.
    let (status, body) = post(
        port,
        "/api/dry-run",
        r#"{"prompt":"fix the flaky test","options":{"report_only":true}}"#,
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""type":"triaged""#));
    assert!(h.ledger()?.is_empty());

    // Submit without options: no options key in the stored event.
    let (status, body) = post(port, "/api/submit", r#"{"prompt":"plain job"}"#)?;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(r#""type":"submitted""#));
    let submitted = h
        .ledger()?
        .into_iter()
        .find(|r| r.kind == "job-submitted")
        .ok_or("no job-submitted")?;
    assert!(submitted.payload.get("options").is_none());

    // Submit with options persists them.
    let (status, _) = post(
        port,
        "/api/submit",
        r#"{"prompt":"with options","options":{"max_stages":2,"risk_flags":["touches-auth"]}}"#,
    )?;
    assert_eq!(status, 200);
    let with = h
        .ledger()?
        .into_iter()
        .filter(|r| r.kind == "job-submitted")
        .find(|r| r.payload["prompt"] == "with options")
        .ok_or("missing")?;
    assert_eq!(with.payload["options"]["max_stages"], 2);

    // Usage errors are 400 with the Response::Error body.
    for bad in [
        r#"{"prompt":"x","options":{"max_stages":0}}"#,
        r#"{"prompt":"x","options":{"context_hint":"  "}}"#,
        r#"{"prompt":"  "}"#,
    ] {
        let (status, body) = post(port, "/api/submit", bad)?;
        assert_eq!(status, 400, "{bad}: {body}");
        assert!(body.contains(r#""kind":"usage""#), "{body}");
    }

    // Rejections change no state.
    let before = h.ledger()?.len();
    let json = "Content-Type: application/json";
    let evil_host = "Host: evil.example";
    let evil_origin = format!("Origin: http://evil.example");
    let big = format!(r#"{{"prompt":"{}"}}"#, "a".repeat(70 * 1024));
    let cases: Vec<(u16, TestResult<(u16, String)>)> = vec![
        (
            403,
            http(
                port,
                "POST",
                "/api/submit",
                &[evil_host, json],
                r#"{"prompt":"x"}"#,
            ),
        ),
        (
            403,
            http(
                port,
                "POST",
                "/api/submit",
                &[&host, &evil_origin, json],
                r#"{"prompt":"x"}"#,
            ),
        ),
        (
            415,
            http(
                port,
                "POST",
                "/api/submit",
                &[&host, "Content-Type: text/plain"],
                r#"{"prompt":"x"}"#,
            ),
        ),
        (413, http(port, "POST", "/api/submit", &[&host, json], &big)),
        (404, http(port, "POST", "/api/nope", &[&host, json], "{}")),
        (405, http(port, "GET", "/api/submit", &[&host], "")),
    ];
    for (want, got) in cases {
        assert_eq!(got?.0, want);
    }
    assert_eq!(h.ledger()?.len(), before);
    Ok(())
}

#[test]
fn http_is_off_by_default_and_a_non_loopback_address_is_a_config_error() -> TestResult {
    let h = Harness::new()?;
    let repo = h.repo()?;
    let cli = h.run(&[
        "--dry-run",
        "--json",
        "--repo",
        &repo.to_string_lossy(),
        "x",
    ])?;
    assert_eq!(cli.code, 0, "{}", cli.stderr);

    let h = Harness::new()?.with_env("PIZZA_DELIVERY_HTTP_LISTEN", "0.0.0.0:7878");
    let cli = h.run(&["daemon"])?;
    assert_ne!(cli.code, 0);
    assert!(cli.stderr.contains("loopback"), "{}", cli.stderr);
    Ok(())
}
