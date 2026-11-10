mod harness;

use harness::access::{claims, CertsServer, Signer, AUD, OWNER};
use harness::{Harness, TestResult};
use serde_json::json;

const ASSERTION: &str = "Cf-Access-Jwt-Assertion";
const RAY: (&str, &str) = ("Cf-Ray", "8a1b2c3d4e5f-AMS");

fn remote(certs: &CertsServer) -> TestResult<Harness> {
    Ok(Harness::new()?.with_remote(&certs.url))
}

fn status_of(h: &Harness, headers: &[(&str, &str)]) -> TestResult<u16> {
    Ok(h.http_with("GET", "/api/jobs", headers, None)?.0)
}

fn with(overrides: serde_json::Value) -> serde_json::Value {
    let mut base = claims(OWNER);
    if let (Some(base), Some(extra)) = (base.as_object_mut(), overrides.as_object()) {
        for (k, v) in extra {
            if v.is_null() {
                base.remove(k);
            } else {
                base.insert(k.clone(), v.clone());
            }
        }
    }
    base
}

#[test]
fn a_remote_request_needs_a_valid_access_assertion_while_local_stays_trusted() -> TestResult {
    let signer = Signer::one();
    let certs = CertsServer::start(&[("k1", &signer)])?;
    let h = remote(&certs)?;
    let valid = signer.sign("k1", &claims(OWNER));

    assert_eq!(status_of(&h, &[])?, 200);
    assert_eq!(status_of(&h, &[RAY])?, 401);
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, "garbage")])?, 401);
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, &valid)])?, 200);
    Ok(())
}

#[test]
fn every_way_an_assertion_can_be_wrong_is_a_401() -> TestResult {
    let signer = Signer::one();
    let other = Signer::two();
    let certs = CertsServer::start(&[("k1", &signer)])?;
    let h = remote(&certs)?;
    let now = harness::access::now();
    let bad = [
        signer.sign("k1", &with(json!({"aud": ["someone-else"]}))),
        signer.sign("k1", &with(json!({"aud": null}))),
        signer.sign("k1", &with(json!({"iss": "https://evil.example.com"}))),
        signer.sign("k1", &with(json!({"exp": now - 10}))),
        signer.sign("k1", &with(json!({"exp": null}))),
        signer.sign("k1", &with(json!({"email": "intruder@example.com"}))),
        signer.sign("k1", &with(json!({"email": null}))),
        other.sign("k1", &claims(OWNER)),
        signer.sign("nope", &claims(OWNER)),
        signer.sign_with_alg("k1", "none", &claims(OWNER)),
        signer.sign_with_alg("k1", "HS256", &claims(OWNER)),
    ];
    for token in &bad {
        assert_eq!(status_of(&h, &[RAY, (ASSERTION, token)])?, 401, "{token}");
    }
    let good = signer.sign(
        "k1",
        &with(json!({"email": "OWNER@Example.com", "aud": AUD})),
    );
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, &good)])?, 200);
    Ok(())
}

#[test]
fn an_unknown_key_id_refreshes_the_cached_keys() -> TestResult {
    let (old, new) = (Signer::one(), Signer::two());
    let certs = CertsServer::start(&[("k1", &old)])?;
    let h = Harness::new()?
        .with_remote(&certs.url)
        .with_env("PIZZA_DELIVERY_ACCESS_REFRESH_MIN_MS", "0");
    let first = old.sign("k1", &claims(OWNER));
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, &first)])?, 200);
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, &first)])?, 200);
    assert_eq!(certs.hits(), 1);

    certs.rotate(&[("k2", &new)]);
    let rotated = new.sign("k2", &claims(OWNER));
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, &rotated)])?, 200);
    assert_eq!(certs.hits(), 2);
    Ok(())
}

#[test]
fn unsafe_remote_configuration_fails_startup() -> TestResult {
    let certs = CertsServer::start(&[])?;
    for missing in ["PIZZA_DELIVERY_ACCESS_EMAILS", "PIZZA_DELIVERY_ACCESS_AUD"] {
        let h = Harness::new()?
            .with_remote(&certs.url)
            .with_env(missing, "");
        let out = h.run(&["status"])?;
        assert_ne!(out.code, 0, "{missing}");
        assert!(out.stderr.contains("remote access"), "{}", out.stderr);
    }
    Ok(())
}

const CLIENT: (&str, &str) = ("X-Pizza-Client", "web");
const ORIGIN: (&str, &str) = ("Origin", "https://pizza.example.com");
const HOST: (&str, &str) = ("X-Forwarded-Host", "pizza.example.com");

fn submit(h: &Harness, headers: &[(&str, &str)]) -> TestResult<(u16, serde_json::Value)> {
    let repo = h.repo()?.to_string_lossy().into_owned();
    h.http_with(
        "POST",
        "/api/jobs",
        headers,
        Some(&json!({"prompt": "remote work", "repo": repo})),
    )
}

#[test]
fn remote_writes_need_a_matching_origin_the_client_header_and_json() -> TestResult {
    let signer = Signer::one();
    let certs = CertsServer::start(&[("k1", &signer)])?;
    let h = remote(&certs)?;
    let token = signer.sign("k1", &claims(OWNER));
    let auth = (ASSERTION, token.as_str());

    let rejected: [&[(&str, &str)]; 5] = [
        &[RAY, auth, HOST],
        &[RAY, auth, HOST, ORIGIN],
        &[RAY, auth, HOST, CLIENT],
        &[
            RAY,
            auth,
            HOST,
            CLIENT,
            ("Origin", "https://evil.example.com"),
        ],
        &[RAY, auth, CLIENT, ORIGIN],
    ];
    for headers in rejected {
        assert_eq!(submit(&h, headers)?.0, 403, "{headers:?}");
    }
    let (status, body) = submit(&h, &[RAY, auth, HOST, CLIENT, ORIGIN])?;
    assert_eq!(status, 200, "{body}");

    let (status, _) = h.http_with(
        "POST",
        "/api/jobs",
        &[
            RAY,
            auth,
            HOST,
            CLIENT,
            ORIGIN,
            ("Content-Type", "text/plain"),
        ],
        Some(&json!({"prompt": "x"})),
    )?;
    assert_eq!(status, 415);

    let (status, _) = h.http_with("OPTIONS", "/api/jobs", &[RAY, auth, HOST, ORIGIN], None)?;
    assert_eq!(status, 403);
    let (_, head, _) = h.raw_get("/api/jobs")?;
    assert!(
        !head.to_ascii_lowercase().contains("access-control"),
        "{head}"
    );
    Ok(())
}

#[test]
fn remote_submits_can_be_disabled_or_rate_limited_without_touching_local_use() -> TestResult {
    let signer = Signer::one();
    let certs = CertsServer::start(&[("k1", &signer)])?;
    let token = signer.sign("k1", &claims(OWNER));
    let headers = [RAY, (ASSERTION, token.as_str()), HOST, CLIENT, ORIGIN];

    let disabled = remote(&certs)?.with_env("PIZZA_DELIVERY_REMOTE_SUBMITS", "off");
    assert_eq!(submit(&disabled, &headers)?.0, 403);
    assert_eq!(submit(&disabled, &[])?.0, 200);

    let limited = remote(&certs)?.with_env("PIZZA_DELIVERY_REMOTE_SUBMIT_LIMIT", "2");
    assert_eq!(submit(&limited, &headers)?.0, 200);
    assert_eq!(submit(&limited, &headers)?.0, 200);
    assert_eq!(submit(&limited, &headers)?.0, 429);
    assert_eq!(submit(&limited, &[])?.0, 200);
    Ok(())
}

#[test]
fn remote_actions_and_rejections_are_recorded_in_the_ledger() -> TestResult {
    let signer = Signer::one();
    let certs = CertsServer::start(&[("k1", &signer)])?;
    let h = remote(&certs)?.with_env("PIZZA_DELIVERY_AUTH_REJECT_WINDOW_MS", "2500");
    let token = signer.sign("k1", &claims(OWNER));
    let headers = [RAY, (ASSERTION, token.as_str()), HOST, CLIENT, ORIGIN];

    let (_, submitted) = submit(&h, &headers)?;
    let id = submitted["id"].as_str().ok_or("no id")?.to_owned();
    for _ in 0..3 {
        assert_eq!(status_of(&h, &[RAY])?, 401);
    }
    assert_eq!(status_of(&h, &[RAY, (ASSERTION, "junk")])?, 401);

    let rows = h.ledger()?;
    let action = rows
        .iter()
        .find(|r| r.kind == "remote-action")
        .ok_or("no remote-action event")?;
    assert_eq!(action.actor, "remote-human");
    assert_eq!(action.job_id, id);
    assert_eq!(action.payload["email"], OWNER);
    assert_eq!(action.payload["cf_ray"], RAY.1);
    assert_eq!(action.payload["action"], "submit");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let rejected = loop {
        if let Some(row) = h.ledger()?.into_iter().find(|r| r.kind == "auth-rejected") {
            break row;
        }
        if std::time::Instant::now() > deadline {
            return Err("no auth-rejected event".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(rejected.payload["count"], 4);
    assert_eq!(rejected.payload["reasons"]["missing-assertion"], 3);
    assert_eq!(rejected.payload["reasons"]["malformed-assertion"], 1);
    Ok(())
}
