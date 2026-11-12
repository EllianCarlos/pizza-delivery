use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pizza_delivery_core::api::{ApiError, ErrorKind};
use pizza_delivery_core::schedule::Priority;
use pizza_delivery_core::triage::TriageOptions;
use pizza_delivery_core::view::{
    board_view, events_view, job_detail, job_full, jobs_view, quota_panel, DismissedView,
    InboxView, RepliedView, SubmittedView,
};
use pizza_delivery_core::{EventPayload, JobState, Request, Response, Store};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use pizza_delivery_core::event::SYSTEM_JOB;
use pizza_delivery_core::{Actor, JobId, NewEvent};

use crate::handler::{handle, Env};
use crate::lock::acquire;
use crate::push::{endpoint_allowed, Push, Subscription};
use crate::remote::{self, Gate, Origin, Submit};
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
    pub gate: Arc<Gate>,
    pub push: Arc<Push>,
}

struct Req {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    last_event_id: Option<u64>,
    json: bool,
    headers: Vec<(String, String)>,
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
    #[serde(default)]
    options: Option<TriageOptions>,
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
        api.flush_rejections(false);
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
    let origin = match api.gate.authenticate(&req.headers, api.env.now_ms() / 1000) {
        Ok(origin) => origin,
        Err(_) => {
            return write_reply(
                &stream,
                denied(401, "unauthorized", "access assertion required"),
            )
        }
    };
    if req.method == "GET" && req.path == "/api/stream" {
        return stream_events(&stream, api, &req);
    }
    if req.method == "GET" && !req.path.starts_with("/api/") {
        return write_asset(&stream, &req);
    }
    let remote = matches!(origin, Origin::Remote { .. });
    if req.method != "GET" && !remote::write_allowed(&req.headers, remote) {
        return write_reply(
            &stream,
            denied(403, "forbidden", "origin or client header rejected"),
        );
    }
    if remote && req.method == "POST" && creates_job(&req.path) {
        match api.gate.submit() {
            Submit::Allowed => {}
            Submit::Disabled => {
                return write_reply(
                    &stream,
                    denied(403, "forbidden", "remote submits are disabled"),
                )
            }
            Submit::Limited => {
                return write_reply(
                    &stream,
                    denied(429, "rate-limited", "too many remote submits"),
                )
            }
        }
    }
    let reply = route(api, &req);
    if let Origin::Remote { email, ray } = &origin {
        api.audit_remote(email, ray.as_deref(), &req, &reply);
    }
    write_reply(&stream, reply)
}

fn creates_job(path: &str) -> bool {
    matches!(path.trim_end_matches('/'), "/api/jobs" | "/api/inbox/reply")
}

fn denied(status: u16, kind: &str, message: &str) -> Reply {
    Reply {
        status,
        body: serde_json::json!({"kind": kind, "message": message, "matches": []}),
    }
}

fn write_asset(mut stream: &TcpStream, req: &Req) -> std::io::Result<()> {
    let accept = req
        .headers
        .iter()
        .find(|(name, _)| name == "accept-encoding")
        .map_or("", |(_, value)| value.as_str());
    let (status, content_type, body, encoding, cache) =
        match crate::assets::lookup(&req.path, accept) {
            Some(s) => (
                "200 OK",
                s.content_type,
                s.body,
                s.encoding,
                s.cache_control,
            ),
            None => (
                "404 Not Found",
                "text/plain; charset=utf-8",
                b"not found".as_slice(),
                None,
                crate::assets::REVALIDATE,
            ),
        };
    let encoding = encoding.map_or(String::new(), |e| format!("Content-Encoding: {e}\r\n"));
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\n{encoding}Vary: Accept-Encoding\r\nContent-Length: {}\r\nCache-Control: {cache}\r\nConnection: close\r\n\r\n",
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
    let mut headers = Vec::new();
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
            headers.push((name.to_ascii_lowercase(), value.to_owned()));
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
        headers,
        body,
    }))
}

fn status_text(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        401 => "Unauthorized",
        403 => "Forbidden",
        409 => "Conflict",
        429 => "Too Many Requests",
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
    fn append(&self, job: &str, actor: Actor, payload: EventPayload) {
        let event = NewEvent {
            id: self.env.new_ulid(),
            job_id: JobId::new(job),
            stage_id: None,
            ts_ms: self.env.now_ms(),
            actor,
            payload,
        };
        if let Ok(mut guard) = self.store.lock() {
            if let Err(e) = guard.append_event(event) {
                eprintln!("ledger: {e}");
            }
        }
    }

    pub fn flush_rejections(&self, force: bool) {
        if let Some(counts) = self.gate.take_due_rejections(force) {
            let count = counts.values().sum();
            let reasons = counts
                .into_iter()
                .map(|(reject, n)| (reject.as_str().to_owned(), n))
                .collect();
            self.append(
                SYSTEM_JOB,
                Actor::Daemon,
                EventPayload::AuthRejected { count, reasons },
            );
        }
    }

    fn audit_remote(&self, email: &str, ray: Option<&str>, req: &Req, reply: &Reply) {
        if req.method == "POST" && req.path.trim_end_matches('/') == "/api/inbox/dismiss" {
            if reply.status == 200 {
                for job in self.jobs_of_entries(&reply.body) {
                    self.append_remote_action(&job, email, ray, "dismiss");
                }
            }
            return;
        }
        let action = match (req.method.as_str(), req.path.trim_end_matches('/')) {
            ("POST", "/api/jobs") => "submit",
            ("POST", "/api/inbox/reply") => "reply",
            ("POST", path) if path.ends_with("/cancel") => "cancel",
            _ => return,
        };
        let Some(job) = reply.body.get("id").and_then(|id| id.as_str()) else {
            return;
        };
        if reply.status != 200 {
            return;
        }
        self.append_remote_action(job, email, ray, action);
    }

    fn append_remote_action(&self, job: &str, email: &str, ray: Option<&str>, action: &str) {
        self.append(
            job,
            Actor::RemoteHuman,
            EventPayload::RemoteAction {
                email: email.to_owned(),
                cf_ray: ray.map(str::to_owned),
                action: action.to_owned(),
            },
        );
    }

    fn jobs_of_entries(&self, body: &serde_json::Value) -> Vec<String> {
        let dismissed: Vec<&str> = body
            .get("entries")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let Response::Inbox { entries } = self.run(Request::Inbox {
            all: true,
            filter: Default::default(),
            query: None,
            limit: None,
        }) else {
            return Vec::new();
        };
        let mut jobs: Vec<String> = entries
            .iter()
            .filter(|e| dismissed.contains(&e.id.as_str()))
            .map(|e| e.job.as_str().to_owned())
            .collect();
        jobs.sort();
        jobs.dedup();
        jobs
    }

    fn run(&self, request: Request) -> Response {
        match acquire(&self.store, "store") {
            Ok(mut guard) => handle(request, guard.as_mut(), self.env.as_ref()),
            Err(e) => Response::Error {
                error: ApiError {
                    kind: ErrorKind::Internal,
                    message: e.to_string(),
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
        ("GET", ["api", "jobs", reference]) => detail(api, reference, false),
        ("GET", ["api", "jobs", reference, "detail"]) => detail(api, reference, true),
        ("GET", ["api", "jobs", reference, "artifacts", index]) => artifact(api, reference, index),
        ("GET", ["api", "quota"]) => match api.run(Request::Quota) {
            Response::Quota { agents, now_ms } => ok(&quota_panel(agents, now_ms)),
            Response::Error { error } => failed(error),
            _ => bad(ErrorKind::Internal, "unexpected response"),
        },
        ("GET", ["api", "inbox"]) => {
            let all = matches!(query(req, "all"), Some("1" | "true"));
            match api.run(Request::Inbox {
                all,
                filter: Default::default(),
                query: None,
                limit: None,
            }) {
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
        ("GET", ["api", "push", "key"]) => Reply {
            status: 200,
            body: serde_json::json!({"key": api.push.vapid.public_key()}),
        },
        ("POST", ["api", "push", "subscribe"]) => subscribe(api, req),
        ("POST", ["api", "push", "unsubscribe"]) => unsubscribe(api, req),
        ("POST", ["api", "inbox", "reply"]) => reply(api, req),
        ("POST", ["api", "inbox", "dismiss"]) => dismiss(api, req),
        (_, ["api", "jobs"] | ["api", "board" | "quota" | "inbox" | "events" | "stream"]) => {
            method_not_allowed()
        }
        (_, ["api", "jobs", _]) | (_, ["api", "jobs", _, "cancel" | "detail"]) => {
            method_not_allowed()
        }
        (_, ["api", "inbox", "reply" | "dismiss"]) => method_not_allowed(),
        (_, ["api", "push", "key" | "subscribe" | "unsubscribe"]) => method_not_allowed(),
        _ => bad(ErrorKind::NotFound, "no such route"),
    }
}

#[derive(Deserialize)]
struct PushKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubscribeBody {
    endpoint: String,
    keys: PushKeys,
}

#[derive(Deserialize)]
struct UnsubscribeBody {
    endpoint: String,
}

fn subscribe(api: &Api, req: &Req) -> Reply {
    let body: SubscribeBody = match body(req) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    if !endpoint_allowed(&body.endpoint) {
        return bad(ErrorKind::Usage, "push endpoint must be https");
    }
    match api.push.subscriptions.add(Subscription {
        endpoint: body.endpoint,
        p256dh: body.keys.p256dh,
        auth: body.keys.auth,
    }) {
        Ok(()) => Reply {
            status: 200,
            body: serde_json::json!({"subscribed": true}),
        },
        Err(e) => bad(ErrorKind::Internal, e),
    }
}

fn unsubscribe(api: &Api, req: &Req) -> Reply {
    let body: UnsubscribeBody = match body(req) {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    match api.push.subscriptions.remove(&body.endpoint) {
        Ok(()) => Reply {
            status: 200,
            body: serde_json::json!({"subscribed": false}),
        },
        Err(e) => bad(ErrorKind::Internal, e),
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

fn detail(api: &Api, reference: &str, full: bool) -> Reply {
    let job = match api.run(Request::Status {
        job: Some(reference.to_owned()),
    }) {
        Response::Job { job, .. } => job,
        Response::Error { error } => return failed(error),
        _ => return bad(ErrorKind::Internal, "unexpected response"),
    };
    let inbox = match api.run(Request::Inbox {
        all: true,
        filter: Default::default(),
        query: None,
        limit: None,
    }) {
        Response::Inbox { entries } => entries,
        _ => Vec::new(),
    };
    let timeline = match acquire(&api.store, "store") {
        Ok(guard) => guard.read_timeline_cursored(&job.id),
        Err(e) => return bad(ErrorKind::Internal, e.to_string()),
    };
    match timeline {
        Ok(events) if full => ok(&job_full(job, &events, &inbox)),
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
    let timeline = match acquire(&api.store, "store") {
        Ok(guard) => guard.read_timeline_cursored(&job.id),
        Err(e) => return bad(ErrorKind::Internal, e.to_string()),
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
    let read = match acquire(&api.store, "store") {
        Ok(guard) => guard.events_since(since, limit),
        Err(e) => return bad(ErrorKind::Internal, e.to_string()),
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
        options: b.options,
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
