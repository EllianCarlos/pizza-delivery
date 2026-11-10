use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use rusqlite::{Connection, OpenFlags};
use tempfile::TempDir;

pub type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const FAKE_AGENT: &str = "#!/bin/sh\necho '{\"type\":\"result\",\"is_error\":false}'\n";

pub struct Harness {
    root: TempDir,
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
        let root = TempDir::new()?;
        let agent = root.path().join("fake-agent");
        fs::write(&agent, FAKE_AGENT)?;
        fs::set_permissions(&agent, fs::Permissions::from_mode(0o755))?;
        Ok(Self { root })
    }

    pub fn db_path(&self) -> PathBuf {
        self.root.path().join("data").join("ledger.sqlite3")
    }

    pub fn run(&self, args: &[&str]) -> TestResult<Cli> {
        let out: Output = Command::new(env!("CARGO_BIN_EXE_pizza-delivery"))
            .args(args)
            .env("PIZZA_DELIVERY_DATA_DIR", self.root.path().join("data"))
            .env("PIZZA_DELIVERY_SOCKET", self.root.path().join("pd.sock"))
            .env("PIZZA_DELIVERY_AGENT", self.root.path().join("fake-agent"))
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
        let cli = self.run(&[prompt])?;
        let lines = cli.json_lines()?;
        let id = lines
            .first()
            .and_then(|v| v["id"].as_str())
            .ok_or("submit printed no id")?;
        Ok(id.to_owned())
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
