use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::process::{kill_group, terminate_group};

pub struct Invocation {
    pub prompt: String,
    pub cwd: PathBuf,
    pub handoff_path: PathBuf,
    pub timeout: Duration,
}

pub struct AgentOutput {
    pub stdout: String,
    pub timed_out: bool,
}

pub trait Agent: Send + Sync {
    fn name(&self) -> &str;
    fn run(&self, invocation: &Invocation, on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput>;
}

pub struct ClaudeAgent {
    command: PathBuf,
}

impl ClaudeAgent {
    pub fn new(command: &Path) -> Self {
        Self {
            command: command.to_owned(),
        }
    }
}

const GRACE: Duration = Duration::from_secs(5);

impl Agent for ClaudeAgent {
    fn name(&self) -> &str {
        "claude"
    }

    fn run(&self, inv: &Invocation, on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput> {
        let mut child = Command::new(&self.command)
            .args([
                "-p",
                &inv.prompt,
                "--output-format",
                "stream-json",
                "--verbose",
            ])
            .current_dir(&inv.cwd)
            .env("PIZZA_DELIVERY_HANDOFF", &inv.handoff_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .with_context(|| format!("spawn {}", self.command.display()))?;
        let pid = child.id();
        on_spawn(pid);
        let mut pipe = child.stdout.take().context("agent stdout")?;
        let reader = std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        });
        let started = Instant::now();
        let mut timed_out = false;
        loop {
            if child.try_wait().context("wait for agent")?.is_some() {
                break;
            }
            if started.elapsed() >= inv.timeout {
                timed_out = true;
                terminate_group(pid);
                let grace_started = Instant::now();
                while grace_started.elapsed() < GRACE
                    && child.try_wait().context("wait for agent")?.is_none()
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                kill_group(pid);
                let _ = child.wait();
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let stdout = reader.join().unwrap_or_default();
        Ok(AgentOutput { stdout, timed_out })
    }
}
