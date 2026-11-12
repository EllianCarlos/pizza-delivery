use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use pizza_delivery_core::mcp::{Activation, Broker, Catalog, CatalogEntry, McpEvent};
use serde_json::{json, Value};

const CHILD_TIMEOUT: Duration = Duration::from_secs(30);
const TICK: Duration = Duration::from_millis(500);

pub struct Settings {
    pub catalog: Catalog,
    pub stage: String,
    pub agent: String,
    pub log: PathBuf,
    pub secrets: Option<PathBuf>,
}

struct ChildServer {
    process: Child,
    stdin: ChildStdin,
    lines: Receiver<Value>,
    next_id: u64,
    tools: Vec<Value>,
}

impl ChildServer {
    fn send(&mut self, message: &Value) -> Result<()> {
        writeln!(self.stdin, "{message}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let message = self
                .lines
                .recv_timeout(CHILD_TIMEOUT)
                .map_err(|_| anyhow!("server did not answer {method}"))?;
            if message.get("id") == Some(&json!(id)) {
                if let Some(error) = message.get("error") {
                    return Err(anyhow!("server error: {error}"));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    fn stop(mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn credential(secrets: Option<&Path>, name: &str) -> Option<String> {
    let dir = secrets?;
    let safe = !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..";
    if !safe {
        return None;
    }
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|v| v.trim_end_matches('\n').to_owned())
}

fn launch(entry: &CatalogEntry, secrets: Option<&Path>) -> Result<ChildServer> {
    let (program, args) = entry
        .command
        .split_first()
        .ok_or_else(|| anyhow!("url servers are not supported by the stdio broker yet"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("PIZZA_DELIVERY_SECRETS");
    for name in &entry.credentials {
        let value = credential(secrets, name)
            .ok_or_else(|| anyhow!("credential '{name}' is not available"))?;
        command.env(name, value);
    }
    let mut process = command
        .spawn()
        .with_context(|| format!("start {}", entry.name))?;
    let stdin = process.stdin.take().context("server stdin")?;
    let stdout = process.stdout.take().context("server stdout")?;
    let (tx, lines) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if tx.send(value).is_err() {
                    break;
                }
            }
        }
    });
    let mut server = ChildServer {
        process,
        stdin,
        lines,
        next_id: 0,
        tools: Vec::new(),
    };
    let handshake = (|| -> Result<Vec<Value>> {
        server.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "pizza-delivery-broker", "version": "0"}
            }),
        )?;
        server.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))?;
        let listed = server.request("tools/list", json!({}))?;
        Ok(listed
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    })();
    match handshake {
        Ok(tools) => {
            server.tools = tools;
            Ok(server)
        }
        Err(e) => {
            server.stop();
            Err(e)
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or_default()
}

fn text_result(text: impl Into<String>, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text.into()}], "isError": is_error})
}

fn broker_tools() -> Vec<Value> {
    let object = |properties: Value, required: &[&str]| json!({"type": "object", "properties": properties, "required": required});
    vec![
        json!({
            "name": "mcp_list",
            "description": "List the MCP servers you may activate for this stage",
            "inputSchema": object(json!({}), &[])
        }),
        json!({
            "name": "mcp_activate",
            "description": "Activate a catalog MCP server; its tools appear as <server>__<tool> and are always reachable through mcp_call",
            "inputSchema": object(json!({"name": {"type": "string"}}), &["name"])
        }),
        json!({
            "name": "mcp_deactivate",
            "description": "Deactivate an MCP server you no longer need",
            "inputSchema": object(json!({"name": {"type": "string"}}), &["name"])
        }),
        json!({
            "name": "mcp_call",
            "description": "Call a tool of an activated MCP server",
            "inputSchema": object(json!({
                "server": {"type": "string"},
                "tool": {"type": "string"},
                "arguments": {"type": "object"}
            }), &["server", "tool"])
        }),
    ]
}

struct Session {
    broker: Broker,
    children: BTreeMap<String, ChildServer>,
    log: PathBuf,
    secrets: Option<PathBuf>,
}

impl Session {
    fn flush(&mut self) {
        let events = self.broker.take_events();
        if events.is_empty() {
            return;
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.log) {
            for event in &events {
                if let Ok(line) = serde_json::to_string(event) {
                    let _ = writeln!(file, "{line}");
                }
            }
        }
    }

    fn drop_children(&mut self, names: &[String]) {
        for name in names {
            if let Some(child) = self.children.remove(name) {
                child.stop();
            }
        }
    }

    fn tools(&self) -> Vec<Value> {
        let mut tools = broker_tools();
        for (server, child) in &self.children {
            for tool in &child.tools {
                let mut tool = tool.clone();
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    tool["name"] = json!(format!("{server}__{name}"));
                }
                tools.push(tool);
            }
        }
        tools
    }

    fn activate(&mut self, name: &str) -> (Value, bool) {
        match self.broker.activate(name, now_ms()) {
            Activation::Denied(reason) => (text_result(format!("denied: {reason}"), true), false),
            Activation::Already => (
                text_result(format!("'{name}' is already active"), false),
                false,
            ),
            Activation::Granted(entry) => match launch(&entry, self.secrets.as_deref()) {
                Ok(child) => {
                    let tools: Vec<String> = child
                        .tools
                        .iter()
                        .filter_map(|t| t.get("name").and_then(Value::as_str))
                        .map(|t| format!("{name}__{t}"))
                        .collect();
                    self.children.insert(name.to_owned(), child);
                    (
                        text_result(
                            format!("activated '{name}'; tools: {}", tools.join(", ")),
                            false,
                        ),
                        true,
                    )
                }
                Err(e) => {
                    self.broker.deactivate(name, "launch-failed");
                    (
                        text_result(format!("could not start '{name}': {e}"), true),
                        false,
                    )
                }
            },
        }
    }

    fn call(&mut self, server: &str, tool: &str, arguments: Value) -> Value {
        if !self.broker.record_call(server, now_ms()) {
            return text_result(format!("'{server}' is not active"), true);
        }
        let Some(child) = self.children.get_mut(server) else {
            return text_result(format!("'{server}' is not running"), true);
        };
        match child.request("tools/call", json!({"name": tool, "arguments": arguments})) {
            Ok(result) => result,
            Err(e) => text_result(format!("call failed: {e}"), true),
        }
    }

    fn tool_call(&mut self, name: &str, args: &Value) -> (Value, bool) {
        let string = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
        match name {
            "mcp_list" => (
                text_result(
                    serde_json::to_string(&self.broker.list()).unwrap_or_default(),
                    false,
                ),
                false,
            ),
            "mcp_activate" => self.activate(string("name")),
            "mcp_deactivate" => {
                let target = string("name").to_owned();
                if self.broker.deactivate(&target, "explicit") {
                    self.drop_children(std::slice::from_ref(&target));
                    (text_result(format!("deactivated '{target}'"), false), true)
                } else {
                    (
                        text_result(format!("'{target}' is not active"), true),
                        false,
                    )
                }
            }
            "mcp_call" => (
                self.call(
                    string("server"),
                    string("tool"),
                    args.get("arguments").cloned().unwrap_or_else(|| json!({})),
                ),
                false,
            ),
            other => match other.split_once("__") {
                Some((server, tool)) => (
                    self.call(
                        server,
                        tool,
                        args.get("arguments")
                            .cloned()
                            .unwrap_or_else(|| args.clone()),
                    ),
                    false,
                ),
                None => (text_result(format!("unknown tool '{other}'"), true), false),
            },
        }
    }

    fn handle(&mut self, message: &Value) -> (Option<Value>, bool) {
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(id) = message.get("id").cloned() else {
            return (None, false);
        };
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let (result, changed) = match method {
            "initialize" => (
                json!({
                    "protocolVersion": params
                        .get("protocolVersion")
                        .cloned()
                        .unwrap_or_else(|| json!("2025-06-18")),
                    "capabilities": {"tools": {"listChanged": true}},
                    "serverInfo": {"name": "pizza-delivery-broker", "version": "0"}
                }),
                false,
            ),
            "ping" => (json!({}), false),
            "tools/list" => (json!({"tools": self.tools()}), false),
            "tools/call" => {
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                self.tool_call(name, &args)
            }
            _ => {
                return (
                    Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32601, "message": format!("unknown method {method}")}
                    })),
                    false,
                )
            }
        };
        self.flush();
        (
            Some(json!({"jsonrpc": "2.0", "id": id, "result": result})),
            changed,
        )
    }

    fn expire(&mut self) -> bool {
        let expired = self.broker.expire(now_ms());
        self.drop_children(&expired);
        self.flush();
        !expired.is_empty()
    }

    fn shutdown(&mut self) {
        let names = self.broker.end_stage();
        self.drop_children(&names);
        self.flush();
    }
}

fn emit(output: &mut impl Write, message: &Value) -> Result<()> {
    writeln!(output, "{message}")?;
    output.flush()?;
    Ok(())
}

pub fn serve(
    settings: Settings,
    input: impl BufRead + Send + 'static,
    mut output: impl Write,
) -> Result<()> {
    let mut session = Session {
        broker: Broker::new(settings.catalog, &settings.stage, &settings.agent),
        children: BTreeMap::new(),
        log: settings.log,
        secrets: settings.secrets,
    };
    let (tx, lines) = channel::<String>();
    std::thread::spawn(move || {
        for line in input.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        match lines.recv_timeout(TICK) {
            Ok(line) => {
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let (reply, changed) = session.handle(&message);
                if let Some(reply) = reply {
                    emit(&mut output, &reply)?;
                }
                if changed {
                    emit(
                        &mut output,
                        &json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                    )?;
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if session.expire() {
                    emit(
                        &mut output,
                        &json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}),
                    )?;
                }
            }
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    session.shutdown();
    Ok(())
}

pub fn read_events(log: &Path) -> Vec<McpEvent> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}
