#![allow(dead_code)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

pub const VALID_HANDOFF_AGENT: &str = r#"#!/bin/sh
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

pub struct Harness {
    root: TempDir,
    envs: Vec<(String, String)>,
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
            envs: Vec::new(),
        })
    }

    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.envs.push((key.to_owned(), value.to_owned()));
        self
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
        let repo = self.root.path().join("src").join("my-repo");
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
        let out: Output = Command::new(env!("CARGO_BIN_EXE_pizza-delivery"))
            .args(args)
            .env("PIZZA_DELIVERY_DATA_DIR", self.data_dir())
            .env("PIZZA_DELIVERY_SOCKET", self.root.path().join("pd.sock"))
            .env("PIZZA_DELIVERY_AGENT", self.root.path().join("fake-agent"))
            .envs(self.envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
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

    pub fn raw_db(&self) -> TestResult<Connection> {
        Ok(Connection::open(self.db_path())?)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.run(&["stop"]);
    }
}
