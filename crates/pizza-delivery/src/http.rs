use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::schedule::Priority;
use pizza_delivery_core::view::{
    board_view, events_view, job_detail, jobs_view, quota_panel, DismissedView, InboxView,
    RepliedView, SubmittedView,
};
use pizza_delivery_core::{EventPayload, JobState, Request, Response, Store};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::handler::{handle, Env};
use crate::server::dispatch;

const MAX_BODY: usize = 1 << 20;
const POLL: Duration = Duration::from_millis(100);
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const BATCH: usize = 200;

#[derive(Clone)]
pub struct Api {
    pub store: Arc<Mutex<Box<dyn Store>>>,
    pub env: Arc<dyn Env>,
    pub stop: Arc<AtomicBool>,
    pub kill_grace: Duration,
    pub data_dir: PathBuf,
}

struct Req {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    last_event_id: Option<u64>,
    json: bool,
    body: Vec<u8>,
}

struct Reply {
    status: u16,
    body: serde_json::Value,
}

#[derive(Deserialize)]
struct SubmitBody {
    prompt: String,
    repo: Option<String>,
    priority: Option<Priority>,
}

#[derive(Deserialize)]
struct ReplyBody {
    targets: Vec<String>,
    text: String,
    priority: Option<Priority>,
}

#[derive(Deserialize)]
struct DismissBody {
    entries: Vec<String>,
}

pub fn listen(listener: TcpListener, api: Api) {
    if listener.set_nonblocking(true).is_err() {
        return;
    }
    while !api.stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let api = api.clone();
                std::thread::spawn(move || {
                    let _ = connection(stream, &api);
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(_) => return,
        }
    }
}

fn connection(stream: TcpStream, api: &Api) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    stream.set_write_timeout(Some(Duration::from_secs(10)))?;
    let Some(req) = read_request(&stream)? else {
        return write_reply(&stream, bad(ErrorKind::Usage, "malformed request"));
    };
    if req.method == "GET" && req.path == "/api/stream" {
        return stream_events(&stream, api, &req);
    }
    if req.method == "GET" && !req.path.starts_with("/api/") {
        return write_asset(&stream, &req.path);
    }
    write_reply(&stream, route(api, &req))
}

fn write_asset(mut stream: &TcpStream, path: &str) -> std::io::Result<()> {
    let (status, content_type, body) = match crate::assets::lookup(path) {
        Some((content_type, body)) => ("200 OK", content_type, body),
        None => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found".as_slice(),
        ),
    };
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn read_request(stream: &TcpStream) -> std::io::Result<Option<Req>> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let (path, raw_query) = target.split_once('?').unwrap_or((target, ""));
    let query = raw_query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (k.to_owned(), v.to_owned())
        })
        .collect();
    let (mut length, mut last_event_id, mut json) = (0usize, None, false);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 {
            return Ok(None);
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.parse().unwrap_or(0),
                "last-event-id" => last_event_id = value.parse().ok(),
                "content-type" => json = value.to_ascii_lowercase().starts_with("application/json"),
                _ => {}
            }
        }
    }
    if length > MAX_BODY {
        return Ok(None);
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(Req {
        method: method.to_owned(),
        path: path.to_owned(),
        query,
        last_event_id,
        json,
        body,
    }))
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        415 => "Unsupported Media Type",
        _ => "Internal Server Error",
    }
}

fn write_reply(mut stream: &TcpStream, reply: Reply) -> std::io::Result<()> {
    let body = reply.body.to_string();
    write!(
        stream,
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        reply.status,
        status_text(reply.status),
        body.len()
    )
}

fn ok(value: &impl Serialize) -> Reply {
    match serde_json::to_value(value) {
        Ok(body) => Reply { status: 200, body },
        Err(e) => bad(ErrorKind::Internal, e.to_string()),
    }
}

fn bad(kind: ErrorKind, message: impl Into<String>) -> Reply {
    let status = match kind {
        ErrorKind::NotFound => 404,
        ErrorKind::Ambiguous => 409,
        ErrorKind::Usage => 400,
        ErrorKind::Internal => 500,
    };
    let error = ApiError {
        kind,
        message: message.into(),
        matches: vec![],
    };
    Reply {
        status,
        body: serde_json::to_value(error).unwrap_or(serde_json::Value::Null),
    }
}

fn failed(error: ApiError) -> Reply {
    let mut reply = bad(error.kind, error.message.clone());
    reply.body = serde_json::to_value(&error).unwrap_or(serde_json::Value::Null);
    reply
}

fn query<'a>(req: &'a Req, key: &str) -> Option<&'a str> {
    req.query
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn body<T: DeserializeOwned>(req: &Req) -> Result<T, Reply> {
    if !req.json {
        return Err(Reply {
            status: 415,
            body: serde_json::json!({"kind": "usage", "message": "content-type must be application/json", "matches": []}),
        });
    }
    serde_json::from_slice(&req.body)
        .map_err(|e| bad(ErrorKind::Usage, format!("bad request body: {e}")))
}

impl Api {
    fn run(&self, request: Request) -> Response {
        match self.store.lock() {
            Ok(mut guard) => handle(request, guard.as_mut(), self.env.as_ref()),
            Err(_) => Response::Error {
                error: ApiError {
                    kind: ErrorKind::Internal,
                    message: "store lock poisoned".into(),
                    matches: vec![],
                },
            },
        }
    }

    fn write(&self, request: Request) -> Response {
        dispatch(
            request,
            &self.store,
            self.env.as_ref(),
            &self.stop,
            self.kill_grace,
        )
    }
}

fn route(api: &Api, req: &Req) -> Reply {
    let segments: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    let method = req.method.as_str();
    match (method, segments.as_slice()) {
        ("GET", ["api", "jobs"]) => jobs(api, req),
        ("GET", ["api", "board"]) => match api.run(Request::Status { job: None }) {
            Response::Jobs { jobs } => ok(&board_view(&jobs)),
            Response::Error { error } => failed(error),
            _ => bad(ErrorKind::Internal, "unexpected response"),
        },
        ("GET", ["api", "jobs", reference]) => detail(api, reference),
        ("GET", ["api", "jobs", reference, "artifacts", index]) => artifact(api, reference, index),
        ("GET", ["api", "quota"]) => match api.run(Request::Quota) {
            Response::Quota { agents, now_ms } => ok(&quota_panel(agents, now_ms)),
            Response::Error { error } => failed(error),
            _ => bad(ErrorKind::Internal, "unexpected response"),
        },
        ("GET", ["api", "inbox"]) => {
            let all = matches!(query(req, "all"), Some("1" | "true"));
            match api.run(Request::Inbox { all }) {
                Response::Inbox { entries } => ok(&InboxView { entries }),
                Response::Error { error } => failed(error),
                _ => bad(ErrorKind::Internal, "unexpected response"),
            }
        }
        ("GET", ["api", "events"]) => events(api, req),
        ("POST", ["api", "jobs"]) => submit(api, req),
        ("POST", ["api", "jobs", reference, "cancel"]) => {
            match api.write(Request::Cancel {
                job: (*reference).to_owned(),
            }) {
                Response::Cancelled { id, short_id } => ok(&SubmittedView {
                    id: id.as_str().to_owned(),
                    short_id,
                }),
                Response::Error { error } => failed(error),
                _ => bad(ErrorKind::Internal, "unexpected response"),
            }
        }
        ("POST", ["api", "inbox", "reply"]) => reply(api, req),
        ("POST", ["api", "inbox", "dismiss"]) => dismiss(api, req),
        (_, ["api", "jobs"] | ["api", "board" | "quota" | "inbox" | "events" | "stream"]) => {
            method_not_allowed()
        }
        (_, ["api", "jobs", _]) | (_, ["api", "jobs", _, "cancel"]) => method_not_allowed(),
        (_, ["api", "inbox", "reply" | "dismiss"]) => method_not_allowed(),
        _ => bad(ErrorKind::NotFound, "no such route"),
    }
}

fn method_not_allowed() -> Reply {
    Reply {
        status: 405,
        body: serde_json::json!({"kind": "usage", "message": "method not allowed", "matches": []}),
    }
}

fn jobs(api: &Api, req: &Req) -> Reply {
    let state = match query(req, "state") {
        None => None,
        Some(raw) => match serde_json::from_value::<JobState>(raw.into()) {
            Ok(state) => Some(state),
            Err(_) => return bad(ErrorKind::Usage, format!("unknown state {raw}")),
        },
    };
    match api.run(Request::Status { job: None }) {
        Response::Jobs { jobs } => ok(&jobs_view(&jobs, state)),
        Response::Error { error } => failed(error),
        _ => bad(ErrorKind::Internal, "unexpected response"),
    }
}

fn detail(api: &Api, reference: &str) -> Reply {
    let job = match api.run(Request::Status {
        job: Some(reference.to_owned()),
    }) {
        Response::Job { job, .. } => job,
        Response::Error { error } => return failed(error),
        _ => return bad(ErrorKind::Internal, "unexpected response"),
    };
    let inbox = match api.run(Request::Inbox { all: true }) {
        Response::Inbox { entries } => entries,
        _ => Vec::new(),
    };
    let timeline = match api.store.lock() {
        Ok(guard) => guard.read_timeline_cursored(&job.id),
        Err(_) => return bad(ErrorKind::Internal, "store lock poisoned"),
    };
    match timeline {
        Ok(events) => ok(&job_detail(job, &events, &inbox)),
        Err(e) => bad(ErrorKind::Internal, e.to_string()),
    }
}

fn artifact(api: &Api, reference: &str, index: &str) -> Reply {
    let job = match api.run(Request::Status {
        job: Some(reference.to_owned()),
    }) {
        Response::Job { job, .. } => job,
        Response::Error { error } => return failed(error),
        _ => return bad(ErrorKind::Internal, "unexpected response"),
    };
    let timeline = match api.store.lock() {
        Ok(guard) => guard.read_timeline_cursored(&job.id),
        Err(_) => return bad(ErrorKind::Internal, "store lock poisoned"),
    };
    let events = match timeline {
        Ok(events) => events,
        Err(e) => return bad(ErrorKind::Internal, e.to_string()),
    };
    let path = index.parse::<usize>().ok().and_then(|n| {
        events
            .iter()
            .filter_map(|(_, e)| match &e.payload {
                EventPayload::ArtifactRecorded { path, .. } => Some(path.clone()),
                _ => None,
            })
            .nth(n)
    });
    let Some(path) = path else {
        return bad(ErrorKind::NotFound, "no such artifact");
    };
    if path.starts_with('/') || path.split('/').any(|part| part == "..") {
        return bad(ErrorKind::Usage, "artifact path escapes the data directory");
    }
    match std::fs::read(api.data_dir.join(&path)) {
        Ok(bytes) => ok(&serde_json::json!({
            "path": path,
            "content": String::from_utf8_lossy(&bytes),
        })),
        Err(e) => bad(ErrorKind::NotFound, format!("artifact unreadable: {e}")),
    }
}

fn events(api: &Api, req: &Req) -> Reply {
    let since = query(req, "since")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let limit = query(req, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .map_or(1000, |l| l.clamp(1, 5000));
    let read = match api.store.lock() {
        Ok(guard) => guard.events_since(since, limit),
        Err(_) => return bad(ErrorKind::Internal, "store lock poisoned"),
    };
    match read {
        Ok(entries) => {
            let mut view = events_view(&entries);
            if entries.is_empty() {
                view.next = since;
            }
            ok(&view)
        }
        Err(e) => bad(ErrorKind::Internal, e.to_string()),
    }
}

fn submit(api: &Api, req: &Req) -> Reply {
    let b: SubmitBody = match body(req) {
        Ok(b) => b,
        Err(reply) => return reply,
    };
    match api.write(Request::Submit {
        prompt: b.prompt,
        repo: b.repo,
        priority: b.priority,
    }) {
        Response::Submitted { id, short_id } => ok(&SubmittedView {
            id: id.as_str().to_owned(),
            short_id,
        }),
        Response::Error { error } => failed(error),
        _ => bad(ErrorKind::Internal, "unexpected response"),
    }
}

fn reply(api: &Api, req: &Req) -> Reply {
    let b: ReplyBody = match body(req) {
        Ok(b) => b,
        Err(reply) => return reply,
    };
    match api.write(Request::Reply {
        targets: b.targets,
        text: b.text,
        priority: b.priority,
    }) {
        Response::Replied {
            id,
            short_id,
            answered,
        } => ok(&RepliedView {
            id: id.as_str().to_owned(),
            short_id,
            answered,
        }),
        Response::Error { error } => failed(error),
        _ => bad(ErrorKind::Internal, "unexpected response"),
    }
}

fn dismiss(api: &Api, req: &Req) -> Reply {
    let b: DismissBody = match body(req) {
        Ok(b) => b,
        Err(reply) => return reply,
    };
    match api.write(Request::Dismiss { entries: b.entries }) {
        Response::Dismissed { entries } => ok(&DismissedView { entries }),
        Response::Error { error } => failed(error),
        _ => bad(ErrorKind::Internal, "unexpected response"),
    }
}

fn stream_events(mut stream: &TcpStream, api: &Api, req: &Req) -> std::io::Result<()> {
    let explicit = req
        .last_event_id
        .or_else(|| query(req, "since").and_then(|v| v.parse().ok()));
    let mut cursor = match explicit {
        Some(c) => c,
        None => match api.store.lock().map(|g| g.head()) {
            Ok(Ok(head)) => head,
            _ => 0,
        },
    };
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n: connected\n\n"
    )?;
    stream.flush()?;
    let mut last_write = Instant::now();
    while !api.stop.load(Ordering::SeqCst) {
        let batch = match api.store.lock() {
            Ok(guard) => guard.events_since(cursor, BATCH).unwrap_or_default(),
            Err(_) => return Ok(()),
        };
        if batch.is_empty() {
            if last_write.elapsed() >= KEEP_ALIVE {
                write!(stream, ": keep-alive\n\n")?;
                stream.flush()?;
                last_write = Instant::now();
            }
            std::thread::sleep(POLL);
            continue;
        }
        for (position, event) in &batch {
            let view = pizza_delivery_core::view::event_view(*position, event);
            let data = serde_json::to_string(&view).map_err(std::io::Error::other)?;
            write!(
                stream,
                "id: {position}\nevent: {}\ndata: {data}\n\n",
                view.kind
            )?;
            cursor = *position;
        }
        stream.flush()?;
        last_write = Instant::now();
    }
    Ok(())
}
