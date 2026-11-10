#![allow(dead_code)]

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub const VALID_HANDOFF_AGENT: &str = r#"#!/bin/sh
STAGE_DIR=$(dirname "$PIZZA_DELIVERY_HANDOFF")
STAGE=$(basename "$STAGE_DIR" | sed 's/^[0-9]*-//')
JOB=$(basename "$(dirname "$(dirname "$STAGE_DIR")")")
case "$STAGE" in
grill)
printf '## Round 1\n- Q: which approach?\n  A: the simple one\n  confidence: 90\n' > "$STAGE_DIR/grill.md"
;;
spec)
mkdir -p "docs/specs/$JOB/tickets"
printf '## Problem\np\n\n## Solution\ns\n\n## Acceptance criteria\n- a\n\n## Seams\n- cli\n' > "docs/specs/$JOB/spec.md"
printf -- '---\nid: t1\nblocked_by: []\n---\nbody\n' > "docs/specs/$JOB/tickets/t1.md"
;;
review)
printf '## Findings\nNone\n\n## Summary\nok\n' > "$STAGE_DIR/review.md"
;;
esac
cat > "$PIZZA_DELIVERY_HANDOFF" <<'DOC'
---
job: J
stage: implement
---

## Goal
do the thing

## What was done
did it

## Decisions made
none

## Assumptions
none

## Open questions
none

## Files touched
none

## Instructions for the next stage
review it
DOC
echo '{"type":"result","is_error":false}'
"#;

pub const INVALID_HANDOFF_AGENT: &str = r#"#!/bin/sh
echo "not a handoff" > "$PIZZA_DELIVERY_HANDOFF"
echo '{"type":"result","is_error":false}'
"#;

fn fake_gh_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir =
            std::env::temp_dir().join(format!("pizza-delivery-fake-gh-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let staged = dir.join("gh.tmp");
        let script = "#!/bin/sh\ncase \"$1\" in\npr) echo https://github.com/o/r/pull/1;;\nesac\n";
        if fs::write(&staged, script).is_ok() {
            let _ = fs::set_permissions(&staged, fs::Permissions::from_mode(0o755));
            let _ = fs::rename(&staged, dir.join("gh"));
        }
        dir
    })
}

pub struct Harness {
    root: TempDir,
    env: Vec<(String, String)>,
    routing: Option<PathBuf>,
    http_port: u16,
}

#[derive(Debug)]
pub struct LedgerRow {
    pub job_id: String,
    pub seq: i64,
    pub kind: String,
    pub actor: String,
    pub payload: serde_json::Value,
}

pub struct Cli {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Cli {
    pub fn json_lines(&self) -> TestResult<Vec<serde_json::Value>> {
        self.stdout
            .lines()
            .map(|l| serde_json::from_str(l).map_err(Into::into))
            .collect()
    }
}

impl Harness {
    pub fn new() -> TestResult<Self> {
        Self::with_agent(VALID_HANDOFF_AGENT)
    }

    pub fn with_agent(script: &str) -> TestResult<Self> {
        let root = TempDir::new()?;
        let agent = root.path().join("fake-agent");
        fs::write(&agent, script)?;
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755))?;
        Ok(Self {
            root,
            env: Vec::new(),
            routing: None,
            http_port: TcpListener::bind("127.0.0.1:0")?.local_addr()?.port(),
        })
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_owned(), value.to_owned()));
        self
    }

    pub fn with_routing(scripts: &[(&str, &str)], routing: &str) -> TestResult<Self> {
        let mut harness = Self::with_agent(VALID_HANDOFF_AGENT)?;
        for (name, script) in scripts {
            let path = harness.root.path().join(name);
            fs::write(&path, script)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        let file = harness.root.path().join("routing.toml");
        fs::write(
            &file,
            routing.replace("{root}", &harness.root.path().to_string_lossy()),
        )?;
        harness.routing = Some(file);
        Ok(harness)
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn data_dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir().join("ledger.sqlite3")
    }

    pub fn repo(&self) -> TestResult<PathBuf> {
        self.repo_named("my-repo")
    }

    pub fn repo_named(&self, name: &str) -> TestResult<PathBuf> {
        let repo = self.root.path().join("src").join(name);
        if !repo.exists() {
            fs::create_dir_all(&repo)?;
            let setup: [&[&str]; 2] = [
                &["init", "-q", "-b", "main"],
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    "root",
                ],
            ];
            for args in setup {
                self.vcs(&repo, args)?;
            }
            let remote = self.root.path().join("remotes").join(format!("{name}.git"));
            fs::create_dir_all(&remote)?;
            self.vcs(&remote, &["init", "-q", "--bare"])?;
            self.vcs(
                &repo,
                &["remote", "add", "origin", &remote.to_string_lossy()],
            )?;
        }
        Ok(repo.canonicalize()?)
    }

    pub fn vcs(&self, dir: &Path, args: &[&str]) -> TestResult<String> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }

    pub fn run(&self, args: &[&str]) -> TestResult<Cli> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_pizza-delivery"));
        if let Some(routing) = &self.routing {
            command.env("PIZZA_DELIVERY_ROUTING", routing);
        }
        let path = format!(
            "{}:{}",
            fake_gh_dir().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let out: Output = command
            .args(args)
            .env("PATH", path)
            .env("XDG_CONFIG_HOME", self.root.path().join("xdg"))
            .env("PIZZA_DELIVERY_DATA_DIR", self.data_dir())
            .env("PIZZA_DELIVERY_HTTP_PORT", self.http_port.to_string())
            .env("PIZZA_DELIVERY_SOCKET", self.root.path().join("pd.sock"))
            .env("PIZZA_DELIVERY_AGENT", self.root.path().join("fake-agent"))
            .env("PIZZA_DELIVERY_PROJECTS_DIR", self.root.path().join("src"))
            .env(
                "PIZZA_DELIVERY_ENGRAM_DB",
                self.root.path().join("no-engram.db"),
            )
            .env("PIZZA_DELIVERY_ENGRAM", self.root.path().join("no-engram"))
            .env("PIZZA_DELIVERY_MNEMON", self.root.path().join("no-mnemon"))
            .env("PIZZA_DELIVERY_NOTIFY", "off")
            .envs(self.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .env_remove("NO_COLOR")
            .stdin(Stdio::null())
            .output()?;
        Ok(Cli {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    pub fn submit(&self, prompt: &str) -> TestResult<String> {
        self.id_of(&self.run(&[prompt])?)
    }

    pub fn submit_in_repo(&self, prompt: &str) -> TestResult<String> {
        let repo = self.repo()?;
        self.id_of(&self.run(&["--repo", &repo.to_string_lossy(), prompt])?)
    }

    fn id_of(&self, cli: &Cli) -> TestResult<String> {
        let lines = cli.json_lines()?;
        let id = lines
            .first()
            .and_then(|v| v["id"].as_str())
            .ok_or("submit printed no id")?;
        Ok(id.to_owned())
    }

    pub fn wait_for_state(&self, id: &str, states: &[&str]) -> TestResult<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let cli = self.run(&["status", id])?;
            let job = cli.json_lines()?.into_iter().next().ok_or("no status")?;
            if job["state"].as_str().is_some_and(|s| states.contains(&s)) {
                return Ok(job);
            }
            if Instant::now() > deadline {
                return Err(format!("job did not reach {states:?}: {job}").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn wait_for_end(&self, id: &str) -> TestResult<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let cli = self.run(&["status", id])?;
            let job = cli.json_lines()?.into_iter().next().ok_or("no status")?;
            if matches!(
                job["state"].as_str(),
                Some("completed" | "failed" | "cancelled")
            ) {
                return Ok(job);
            }
            if Instant::now() > deadline {
                return Err(format!("job did not finish: {job}").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn ledger(&self) -> TestResult<Vec<LedgerRow>> {
        let conn = Connection::open_with_flags(self.db_path(), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let mut stmt = conn
            .prepare("SELECT job_id, seq, kind, actor, payload FROM events ORDER BY global_seq")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (job_id, seq, kind, actor, payload) = row?;
            out.push(LedgerRow {
                job_id,
                seq,
                kind,
                actor,
                payload: serde_json::from_str(&payload)?,
            });
        }
        Ok(out)
    }

    pub fn http(
        &self,
        method: &str,
        path: &str,
        body: Option<&serde_json::Value>,
    ) -> TestResult<(u16, serde_json::Value)> {
        self.run(&["status"])?;
        let mut stream = TcpStream::connect(("127.0.0.1", self.http_port))?;
        let payload = body.map(ToString::to_string).unwrap_or_default();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{payload}",
            payload.len()
        )?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (head, text) = raw.split_once("\r\n\r\n").ok_or("malformed response")?;
        let status = head.split_whitespace().nth(1).ok_or("no status")?.parse()?;
        let json = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
        Ok((status, json))
    }

    pub fn raw_get(&self, path: &str) -> TestResult<(u16, String, String)> {
        self.run(&["status"])?;
        let mut stream = TcpStream::connect(("127.0.0.1", self.http_port))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )?;
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        let (head, body) = raw.split_once("\r\n\r\n").ok_or("malformed response")?;
        let status = head.split_whitespace().nth(1).ok_or("no status")?.parse()?;
        Ok((status, head.to_owned(), body.to_owned()))
    }

    pub fn get(&self, path: &str) -> TestResult<serde_json::Value> {
        let (status, body) = self.http("GET", path, None)?;
        assert_eq!(status, 200, "GET {path}: {body}");
        Ok(body)
    }

    pub fn stream(&self, path: &str, last_event_id: Option<u64>) -> TestResult<Sse> {
        self.run(&["status"])?;
        let mut stream = TcpStream::connect(("127.0.0.1", self.http_port))?;
        let resume = last_event_id
            .map(|n| format!("Last-Event-ID: {n}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nAccept: text/event-stream\r\n{resume}\r\n"
        )?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status)?;
        assert!(status.contains("200"), "stream status: {status}");
        let mut content_type = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if line.trim().is_empty() {
                break;
            }
            if line.to_ascii_lowercase().starts_with("content-type:") {
                content_type = line.trim().to_owned();
            }
        }
        assert!(content_type.contains("text/event-stream"), "{content_type}");
        Ok(Sse { reader })
    }

    pub fn raw_db(&self) -> TestResult<Connection> {
        Ok(Connection::open(self.db_path())?)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
    }
}

pub struct Sse {
    reader: BufReader<TcpStream>,
}

pub struct Frame {
    pub id: u64,
    pub event: String,
    pub data: serde_json::Value,
}

impl Sse {
    pub fn next_frame(&mut self) -> TestResult<Frame> {
        let (mut id, mut event, mut data) = (None, String::new(), String::new());
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line)? == 0 {
                return Err("stream closed".into());
            }
            let line = line.trim_end();
            if line.is_empty() {
                if let Some(id) = id {
                    return Ok(Frame {
                        id,
                        event,
                        data: serde_json::from_str(&data)?,
                    });
                }
                continue;
            }
            if let Some(v) = line.strip_prefix("id: ") {
                id = Some(v.parse()?);
            } else if let Some(v) = line.strip_prefix("event: ") {
                event = v.to_owned();
            } else if let Some(v) = line.strip_prefix("data: ") {
                data = v.to_owned();
            }
        }
    }
}
