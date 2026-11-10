mod mcp_mock;

use std::error::Error;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const TOKEN: &str = "s3cr3t-token-value";

const FAKE_PRELUDE: &str = r#"echo "$$" > "$dir/server.pid"
printf '%s' "$FAKE_TOKEN" > "$dir/token-seen""#;

struct Broker {
    dir: TempDir,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
    next_id: u64,
    seen: Vec<String>,
}

fn catalog(dir: &Path, extra: &str) -> String {
    format!(
        r#"
[[server]]
name = "docs"
description = "Library docs"
command = ["{server}"]
credentials = ["FAKE_TOKEN"]
egress = ["docs.example.com"]
auto = true
{extra}

[[server]]
name = "browser"
description = "Browser"
command = ["{server}"]
auto = false

[[server]]
name = "two"
command = ["{server}"]
auto = true

[[server]]
name = "three"
command = ["{server}"]
auto = true

[[server]]
name = "four"
command = ["{server}"]
auto = true
"#,
        server = dir.join("server.sh").display()
    )
}

impl Broker {
    fn start(extra: &str) -> TestResult<Self> {
        let dir = TempDir::new()?;
        let server = dir.path().join("server.sh");
        fs::write(&server, mcp_mock::fake_server(FAKE_PRELUDE))?;
        fs::set_permissions(&server, fs::Permissions::from_mode(0o755))?;
        fs::create_dir(dir.path().join("secrets"))?;
        fs::write(dir.path().join("secrets/FAKE_TOKEN"), format!("{TOKEN}\n"))?;
        fs::write(dir.path().join("catalog.toml"), catalog(dir.path(), extra))?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_pizza-delivery"))
            .arg("mcp-broker")
            .arg("--catalog")
            .arg(dir.path().join("catalog.toml"))
            .args(["--stage", "implement", "--agent", "claude"])
            .arg("--log")
            .arg(dir.path().join("events.jsonl"))
            .arg("--secrets")
            .arg(dir.path().join("secrets"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if tx.send(v).is_err() {
                        break;
                    }
                }
            }
        });
        Ok(Self {
            dir,
            child,
            stdin,
            lines,
            next_id: 0,
            seen: Vec::new(),
        })
    }

    fn send(&mut self, message: Value) -> TestResult {
        let stdin = self.stdin.as_mut().ok_or("stdin closed")?;
        writeln!(stdin, "{message}")?;
        stdin.flush()?;
        Ok(())
    }

    fn next(&mut self, secs: u64) -> TestResult<Value> {
        let v = self.lines.recv_timeout(Duration::from_secs(secs))?;
        self.seen.push(v.to_string());
        Ok(v)
    }

    fn request(&mut self, method: &str, params: Value) -> TestResult<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let v = self.next(30)?;
            if v["id"] == json!(id) {
                return Ok(v["result"].clone());
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> TestResult<Value> {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }

    fn tool_names(&mut self) -> TestResult<Vec<String>> {
        let listed = self.request("tools/list", json!({}))?;
        Ok(listed["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn handshake(&mut self) -> TestResult<Value> {
        let init = self.request("initialize", json!({"protocolVersion": "2025-06-18"}))?;
        self.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        Ok(init)
    }

    fn finish(mut self) -> TestResult<(Vec<Value>, PathBuf)> {
        drop(self.stdin.take());
        self.child.wait()?;
        let log = self.dir.path().join("events.jsonl");
        let events = fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let path = self.dir.path().to_owned();
        std::mem::forget(self.dir);
        Ok((events, path))
    }
}

fn alive(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

fn text(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

#[test]
fn activation_emits_list_changed_and_namespaced_tools_appear_end_to_end() -> TestResult {
    let mut b = Broker::start("")?;
    let init = b.handshake()?;
    assert_eq!(init["capabilities"]["tools"]["listChanged"], true);
    let before = b.tool_names()?;
    assert_eq!(
        before,
        ["mcp_list", "mcp_activate", "mcp_deactivate", "mcp_call"]
    );

    let activated = b.tool("mcp_activate", json!({"name": "docs"}))?;
    assert!(text(&activated).contains("docs__echo"));
    let notice = b.next(5)?;
    assert_eq!(notice["method"], "notifications/tools/list_changed");

    assert!(b.tool_names()?.contains(&"docs__echo".to_owned()));
    assert_eq!(text(&b.tool("docs__echo", json!({}))?), "pong");
    assert_eq!(
        text(&b.tool(
            "mcp_call",
            json!({"server": "docs", "tool": "echo", "arguments": {}})
        )?),
        "pong"
    );

    let deactivated = b.tool("mcp_deactivate", json!({"name": "docs"}))?;
    assert!(text(&deactivated).contains("deactivated"));
    assert_eq!(b.next(5)?["method"], "notifications/tools/list_changed");
    assert!(!b.tool_names()?.contains(&"docs__echo".to_owned()));
    Ok(())
}

#[test]
fn credentials_reach_only_the_server_process_never_the_agent_context() -> TestResult {
    let mut b = Broker::start("")?;
    b.handshake()?;
    b.tool("mcp_list", json!({}))?;
    b.tool("mcp_activate", json!({"name": "docs"}))?;
    b.tool_names()?;
    b.tool("docs__echo", json!({}))?;
    let seen = b.seen.join("\n");
    let dir = b.dir.path().to_owned();
    let (events, _) = b.finish()?;

    assert!(!seen.contains(TOKEN));
    assert!(!seen.contains("FAKE_TOKEN"));
    assert!(!format!("{events:?}").contains(TOKEN));
    assert_eq!(fs::read_to_string(dir.join("token-seen"))?, TOKEN);
    Ok(())
}

#[test]
fn a_denied_request_is_recorded_with_a_feedback_entry() -> TestResult {
    let mut b = Broker::start("")?;
    b.handshake()?;
    let denied = b.tool("mcp_activate", json!({"name": "browser"}))?;
    assert_eq!(denied["isError"], true);
    let unknown = b.tool("mcp_activate", json!({"name": "ghost"}))?;
    assert_eq!(unknown["isError"], true);
    let (events, _) = b.finish()?;

    let denials: Vec<_> = events.iter().filter(|e| e["event"] == "denied").collect();
    assert_eq!(denials.len(), 2);
    assert_eq!(denials[0]["server"], "browser");
    assert_eq!(denials[0]["feedback"]["kind"], "question");
    assert!(denials[1]["reason"]
        .as_str()
        .unwrap_or_default()
        .contains("not in the catalog"));
    Ok(())
}

#[test]
fn only_three_servers_activate_per_stage() -> TestResult {
    let mut b = Broker::start("")?;
    b.handshake()?;
    for name in ["docs", "two", "three"] {
        assert_eq!(
            b.tool("mcp_activate", json!({"name": name}))?["isError"],
            false
        );
    }
    let fourth = b.tool("mcp_activate", json!({"name": "four"}))?;
    assert_eq!(fourth["isError"], true);
    assert!(text(&fourth).contains("at most 3"));
    Ok(())
}

#[test]
fn tool_calls_are_counted_per_server_and_ledgered_on_deactivation() -> TestResult {
    let mut b = Broker::start("")?;
    b.handshake()?;
    b.tool("mcp_activate", json!({"name": "docs"}))?;
    b.tool("docs__echo", json!({}))?;
    b.tool("docs__echo", json!({}))?;
    b.tool("mcp_call", json!({"server": "docs", "tool": "echo"}))?;
    let (events, _) = b.finish()?;

    let calls = events.iter().find(|e| e["event"] == "calls");
    assert_eq!(calls.map(|c| c["count"].clone()), Some(json!(3)));
    let end = events.iter().find(|e| e["event"] == "deactivated");
    assert_eq!(end.map(|e| e["reason"].clone()), Some(json!("stage-end")));
    Ok(())
}

#[test]
fn ending_the_stage_stops_the_server_process() -> TestResult {
    let mut b = Broker::start("")?;
    b.handshake()?;
    b.tool("mcp_activate", json!({"name": "docs"}))?;
    let pid_file = b.dir.path().join("server.pid");
    let pid = fs::read_to_string(&pid_file)?.trim().to_owned();
    assert!(alive(&pid));
    b.finish()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while alive(&pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!alive(&pid));
    Ok(())
}

#[test]
fn an_idle_server_is_stopped_and_the_tool_list_changes_again() -> TestResult {
    let mut b = Broker::start("idle_timeout_secs = 1")?;
    b.handshake()?;
    b.tool("mcp_activate", json!({"name": "docs"}))?;
    assert_eq!(b.next(5)?["method"], "notifications/tools/list_changed");
    assert_eq!(b.next(10)?["method"], "notifications/tools/list_changed");
    assert!(!b.tool_names()?.contains(&"docs__echo".to_owned()));
    let (events, _) = b.finish()?;
    assert!(events
        .iter()
        .any(|e| e["event"] == "deactivated" && e["reason"] == "idle-timeout"));
    Ok(())
}
