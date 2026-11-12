//! Opt-in loopback HTTP/JSON adapter for the web form. It only translates
//! HTTP into the same line-JSON requests the CLI sends over the Unix socket,
//! so every rule (validation, dry run, ledger) stays in one place.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::{Priority, Request, Response, TriageOptions};
use serde::Deserialize;

use crate::client::{self, Start};
use crate::config::Config;

pub const MAX_BODY: usize = 64 * 1024;
const MAX_LINE: u64 = 8 * 1024;
const MAX_HEADERS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(5);

const INDEX_HTML: &str = include_str!("../../../web/index.html");
const FORM_JS: &str = include_str!("../../../web/form.js");

/// Parses `http_listen`; only loopback addresses are accepted.
pub fn parse_listen(value: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = value
        .trim()
        .parse()
        .map_err(|e| format!("not a socket address ({e})"))?;
    if addr.ip().is_loopback() {
        Ok(addr)
    } else {
        Err("must be a loopback address (127.0.0.1 or [::1])".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Internal Server Error",
    }
}

fn read_line(r: &mut impl BufRead) -> Result<String, u16> {
    let mut buf = Vec::new();
    let n = r
        .take(MAX_LINE + 1)
        .read_until(b'\n', &mut buf)
        .map_err(|_| 408u16)?;
    if n == 0 {
        return Err(400);
    }
    if n as u64 > MAX_LINE {
        return Err(431);
    }
    let text = String::from_utf8(buf).map_err(|_| 400u16)?;
    Ok(text.trim_end_matches(['\r', '\n']).to_string())
}

/// Reads one request. The error is the HTTP status to answer with.
pub fn read_request(r: &mut impl BufRead) -> Result<HttpRequest, u16> {
    let start = read_line(r)?;
    let mut parts = start.split(' ');
    let (Some(method), Some(path), Some(version), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(400);
    };
    if !version.starts_with("HTTP/1.") {
        return Err(400);
    }
    let mut headers = Vec::new();
    loop {
        let line = read_line(r)?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(431);
        }
        let (k, v) = line.split_once(':').ok_or(400u16)?;
        headers.push((k.trim().to_string(), v.trim().to_string()));
    }
    let mut request = HttpRequest {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body: Vec::new(),
    };
    if request.header("transfer-encoding").is_some() {
        return Err(400);
    }
    if let Some(len) = request.header("content-length") {
        let len: usize = len.parse().map_err(|_| 400u16)?;
        if len > MAX_BODY {
            return Err(413);
        }
        let mut body = Vec::with_capacity(len);
        r.take(len as u64)
            .read_to_end(&mut body)
            .map_err(|_| 408u16)?;
        if body.len() != len {
            return Err(400);
        }
        request.body = body;
    }
    Ok(request)
}

fn text(status: u16, message: &str) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "text/plain; charset=utf-8",
        body: format!("{message}\n").into_bytes(),
    }
}

fn json(status: u16, response: &Response) -> HttpResponse {
    HttpResponse {
        status,
        content_type: "application/json",
        body: serde_json::to_vec(response).unwrap_or_default(),
    }
}

fn failure(kind: ErrorKind, message: impl Into<String>) -> Response {
    Response::Error {
        error: ApiError {
            kind,
            message: message.into(),
            matches: vec![],
        },
    }
}

fn status_of(response: &Response) -> u16 {
    match response {
        Response::Error { error } => match error.kind {
            ErrorKind::Usage => 400,
            ErrorKind::NotFound | ErrorKind::Ambiguous => 404,
            ErrorKind::Internal => 500,
        },
        _ => 200,
    }
}

fn is_loopback_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(rest) => rest.split(']').next().unwrap_or(""),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1"
}

fn same_loopback_origin(origin: &str, host: &str) -> bool {
    origin
        .strip_prefix("http://")
        .is_some_and(|authority| authority == host && is_loopback_host(authority))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    prompt: String,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    priority: Option<Priority>,
    #[serde(default)]
    options: Option<TriageOptions>,
}

/// Maps one HTTP request to a response; `call` performs the daemon request.
pub fn respond(req: &HttpRequest, call: &dyn Fn(&Request) -> Response) -> HttpResponse {
    let Some(host) = req.header("host").filter(|h| is_loopback_host(h)) else {
        return text(403, "forbidden host");
    };
    if let Some(origin) = req.header("origin") {
        if !same_loopback_origin(origin, host) {
            return text(403, "forbidden origin");
        }
    }
    let path = req.path.split('?').next().unwrap_or("");
    let dry = match path {
        "/" | "/form.js" => {
            if req.method != "GET" {
                return text(405, "method not allowed");
            }
            return if path == "/" {
                HttpResponse {
                    status: 200,
                    content_type: "text/html; charset=utf-8",
                    body: INDEX_HTML.as_bytes().to_vec(),
                }
            } else {
                HttpResponse {
                    status: 200,
                    content_type: "text/javascript; charset=utf-8",
                    body: FORM_JS.as_bytes().to_vec(),
                }
            };
        }
        "/api/submit" => false,
        "/api/dry-run" => true,
        _ => return text(404, "not found"),
    };
    if req.method != "POST" {
        return text(405, "method not allowed");
    }
    let is_json = req.header("content-type").is_some_and(|ct| {
        ct.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"))
    });
    if !is_json {
        return text(415, "content-type must be application/json");
    }
    if req.body.len() > MAX_BODY {
        return text(413, "body too large");
    }
    let body: Body = match serde_json::from_slice(&req.body) {
        Ok(body) => body,
        Err(e) => {
            let response = failure(ErrorKind::Usage, format!("bad request: {e}"));
            return json(400, &response);
        }
    };
    let request = if dry {
        if body.priority.is_some() {
            let response = failure(ErrorKind::Usage, "priority does not apply to a dry run");
            return json(400, &response);
        }
        Request::Triage {
            prompt: body.prompt,
            repo: body.repo,
            options: body.options,
        }
    } else {
        Request::Submit {
            prompt: body.prompt,
            repo: body.repo,
            priority: body.priority,
            options: body.options,
        }
    };
    let response = call(&request);
    json(status_of(&response), &response)
}

fn write_response(stream: &mut TcpStream, r: &HttpResponse) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\
         Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n\
         Content-Security-Policy: default-src 'self'; style-src 'unsafe-inline'\r\n\r\n",
        r.status,
        reason(r.status),
        r.content_type,
        r.body.len()
    )?;
    stream.write_all(&r.body)
}

fn connection(mut stream: TcpStream, config: &Config) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
    let response = match stream.try_clone() {
        Ok(read_half) => match read_request(&mut BufReader::new(read_half)) {
            Ok(req) => respond(&req, &|request| {
                client::send(config, request, Start::Never)
                    .unwrap_or_else(|e| failure(ErrorKind::Internal, e.to_string()))
            }),
            Err(status) => text(status, reason(status)),
        },
        Err(_) => return,
    };
    let _ = write_response(&mut stream, &response);
    // Let the peer finish sending before closing, so an early rejection (such
    // as 413) is read as a response instead of a connection reset.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = std::io::copy(
        &mut (&stream).take(2 * MAX_BODY as u64),
        &mut std::io::sink(),
    );
}

/// Accept loop; runs until `stop` is set.
pub fn serve_http(listener: TcpListener, config: Config, stop: Arc<AtomicBool>) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let config = config.clone();
                std::thread::spawn(move || connection(stream, &config));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn req(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> HttpRequest {
        HttpRequest {
            method: method.into(),
            path: path.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn post(path: &str, body: &str) -> HttpRequest {
        req(
            "POST",
            path,
            &[
                ("Host", "127.0.0.1:7878"),
                ("Content-Type", "application/json"),
            ],
            body,
        )
    }

    fn run(r: &HttpRequest) -> (HttpResponse, Vec<Request>) {
        let seen = RefCell::new(Vec::new());
        let out = respond(r, &|request| {
            seen.borrow_mut().push(request.clone());
            Response::Stopped
        });
        (out, seen.into_inner())
    }

    #[test]
    fn listen_address_must_be_loopback() {
        assert!(parse_listen("127.0.0.1:7878").is_ok());
        assert!(parse_listen("[::1]:7878").is_ok());
        assert!(parse_listen("0.0.0.0:7878").is_err());
        assert!(parse_listen("192.168.1.5:80").is_err());
        assert!(parse_listen("localhost").is_err());
    }

    #[test]
    fn submit_without_options_maps_to_a_plain_submit() {
        let (out, seen) = run(&post("/api/submit", r#"{"prompt":"x"}"#));
        assert_eq!(out.status, 200);
        assert_eq!(
            seen,
            vec![Request::Submit {
                prompt: "x".into(),
                repo: None,
                priority: None,
                options: None
            }]
        );
    }

    #[test]
    fn options_and_repo_pass_through_and_dry_run_maps_to_triage() {
        let body = r#"{"prompt":"x","repo":"/r","options":{"max_stages":3,"report_only":true}}"#;
        let (_, seen) = run(&post("/api/dry-run", body));
        let Some(Request::Triage {
            repo,
            options: Some(o),
            ..
        }) = seen.first()
        else {
            panic!("expected a triage request, got {seen:?}");
        };
        assert_eq!(repo.as_deref(), Some("/r"));
        assert_eq!(o.max_stages, Some(3));
        assert!(o.report_only);
    }

    #[test]
    fn usage_errors_are_400_and_other_errors_follow_their_kind() {
        let usage = respond(&post("/api/submit", r#"{"prompt":""}"#), &|_| {
            failure(ErrorKind::Usage, "prompt must not be empty")
        });
        assert_eq!(usage.status, 400);
        assert!(String::from_utf8_lossy(&usage.body).contains("usage"));
        let internal = respond(&post("/api/submit", r#"{"prompt":"x"}"#), &|_| {
            failure(ErrorKind::Internal, "boom")
        });
        assert_eq!(internal.status, 500);
    }

    #[test]
    fn malformed_json_and_unknown_fields_are_400_and_never_reach_the_daemon() {
        for body in ["nope", r#"{"prompt":"x","extra":1}"#, r#"{"repo":"/r"}"#] {
            let (out, seen) = run(&post("/api/submit", body));
            assert_eq!(out.status, 400, "{body}");
            assert!(seen.is_empty());
        }
    }

    #[test]
    fn foreign_hosts_origins_types_methods_and_paths_are_rejected() {
        let ok_headers = [
            ("Host", "localhost:7878"),
            ("Content-Type", "application/json; charset=utf-8"),
        ];
        let cases = [
            (
                req(
                    "POST",
                    "/api/submit",
                    &[
                        ("Host", "evil.example"),
                        ("Content-Type", "application/json"),
                    ],
                    "{}",
                ),
                403,
            ),
            (
                req(
                    "POST",
                    "/api/submit",
                    &[("Content-Type", "application/json")],
                    "{}",
                ),
                403,
            ),
            (
                req(
                    "POST",
                    "/api/submit",
                    &[
                        ("Host", "localhost:7878"),
                        ("Origin", "http://evil.example"),
                        ("Content-Type", "application/json"),
                    ],
                    "{}",
                ),
                403,
            ),
            (
                req(
                    "POST",
                    "/api/submit",
                    &[("Host", "localhost:7878"), ("Content-Type", "text/plain")],
                    "{}",
                ),
                415,
            ),
            (
                req("POST", "/api/submit", &[("Host", "localhost:7878")], "{}"),
                415,
            ),
            (req("GET", "/api/submit", &ok_headers, ""), 405),
            (req("POST", "/", &ok_headers, "{}"), 405),
            (req("POST", "/nope", &ok_headers, "{}"), 404),
        ];
        for (request, status) in cases {
            let (out, seen) = run(&request);
            assert_eq!(out.status, status, "{request:?}");
            assert!(seen.is_empty(), "{request:?}");
        }
        let same = req(
            "POST",
            "/api/submit",
            &[
                ("Host", "localhost:7878"),
                ("Origin", "http://localhost:7878"),
                ("Content-Type", "application/json"),
            ],
            r#"{"prompt":"x"}"#,
        );
        assert_eq!(run(&same).0.status, 200);
    }

    #[test]
    fn the_page_is_served_with_a_collapsed_additional_parameters_expander() {
        let (out, seen) = run(&req("GET", "/", &[("Host", "127.0.0.1:7878")], ""));
        assert_eq!(out.status, 200);
        assert!(seen.is_empty());
        let html = String::from_utf8_lossy(&out.body);
        assert!(html.contains("Additional Parameters"));
        assert!(html.contains("<details"));
        assert!(!html.contains("<details open"));
        let js = run(&req("GET", "/form.js", &[("Host", "[::1]:7878")], "")).0;
        assert_eq!(js.status, 200);
        assert!(String::from_utf8_lossy(&js.body).contains("buildSubmitBody"));
    }

    #[test]
    fn reading_enforces_the_size_cap_and_rejects_chunked_bodies() {
        let head = |extra: &str| format!("POST /api/submit HTTP/1.1\r\nHost: x\r\n{extra}\r\n");
        let over = head(&format!("Content-Length: {}\r\n", MAX_BODY + 1));
        assert_eq!(read_request(&mut over.as_bytes()), Err(413));
        let chunked = head("Transfer-Encoding: chunked\r\n");
        assert_eq!(read_request(&mut chunked.as_bytes()), Err(400));
        let short = head("Content-Length: 10\r\n") + "abc";
        assert_eq!(read_request(&mut short.as_bytes()), Err(400));
        let good = head("Content-Length: 2\r\n") + "{}";
        let parsed = read_request(&mut good.as_bytes());
        assert_eq!(parsed.map(|r| r.body), Ok(b"{}".to_vec()));
        assert_eq!(read_request(&mut "garbage\r\n\r\n".as_bytes()), Err(400));
    }
}
