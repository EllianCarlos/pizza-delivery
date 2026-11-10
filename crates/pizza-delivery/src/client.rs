use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use pizza_delivery_core::{Request, Response};
use thiserror::Error;

use crate::config::Config;

const START_TIMEOUT: Duration = Duration::from_secs(5);
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("daemon unreachable at {0}")]
    Unreachable(String),
    #[error("protocol error: {0}")]
    Protocol(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    IfNeeded,
    Never,
}

fn connect(config: &Config) -> std::io::Result<UnixStream> {
    UnixStream::connect(&config.socket_path)
}

fn spawn_daemon() -> std::io::Result<()> {
    Command::new(std::env::current_exe()?)
        .arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
}

fn stream(config: &Config, start: Start) -> Result<UnixStream, ClientError> {
    let unreachable = || ClientError::Unreachable(config.socket_path.display().to_string());
    if let Ok(s) = connect(config) {
        return Ok(s);
    }
    if start == Start::Never {
        return Err(unreachable());
    }
    spawn_daemon().map_err(|_| unreachable())?;
    let deadline = Instant::now() + START_TIMEOUT;
    while Instant::now() < deadline {
        if let Ok(s) = connect(config) {
            return Ok(s);
        }
        std::thread::sleep(POLL);
    }
    Err(unreachable())
}

pub fn send(config: &Config, request: &Request, start: Start) -> Result<Response, ClientError> {
    let protocol = |e: &dyn std::fmt::Display| ClientError::Protocol(e.to_string());
    let mut s = stream(config, start)?;
    let body = serde_json::to_string(request).map_err(|e| protocol(&e))?;
    writeln!(s, "{body}").map_err(|e| protocol(&e))?;
    let mut line = String::new();
    BufReader::new(&s)
        .read_line(&mut line)
        .map_err(|e| protocol(&e))?;
    serde_json::from_str(&line).map_err(|e| protocol(&e))
}
