use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use pizza_delivery_core::routing::{AdapterKind, RoutingConfig};

use pizza_delivery_core::sandbox::{sandbox_for, CommandSpec, SandboxProfile};

use crate::process::{kill_group, terminate_group};

pub struct Invocation {
    pub prompt: String,
    pub model: Option<String>,
    pub cwd: PathBuf,
    pub handoff_path: PathBuf,
    pub timeout: Duration,
    pub sandbox: SandboxProfile,
    pub egress_log: PathBuf,
}

pub struct AgentOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

pub trait Agent: Send + Sync {
    fn run(&self, invocation: &Invocation, on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput>;
}

pub fn argv(kind: AdapterKind, prompt: &str, model: Option<&str>) -> Vec<String> {
    let model_flag = |flag: &str| {
        model
            .map(|m| vec![flag.to_owned(), m.to_owned()])
            .unwrap_or_default()
    };
    let strings = |parts: &[&str]| parts.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    match kind {
        AdapterKind::Claude => [
            strings(&["-p", prompt, "--output-format", "stream-json", "--verbose"]),
            model_flag("--model"),
        ]
        .concat(),
        AdapterKind::Opencode => [
            strings(&["run", "--format", "json"]),
            model_flag("-m"),
            strings(&[prompt]),
        ]
        .concat(),
        AdapterKind::Hermes => [
            strings(&["chat", "-q", prompt, "-Q", "--format", "stream-json"]),
            model_flag("--model"),
        ]
        .concat(),
        AdapterKind::Agy => [
            strings(&["-p", prompt, "--output-format", "json"]),
            model_flag("--model"),
        ]
        .concat(),
    }
}

pub struct CliAgent {
    kind: AdapterKind,
    command: PathBuf,
}

impl CliAgent {
    pub fn new(kind: AdapterKind, command: impl Into<PathBuf>) -> Self {
        Self {
            kind,
            command: command.into(),
        }
    }
}

const GRACE: Duration = Duration::from_secs(5);

fn drain(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

impl Agent for CliAgent {
    fn run(&self, inv: &Invocation, on_spawn: &mut dyn FnMut(u32)) -> Result<AgentOutput> {
        let launch = sandbox_for(inv.sandbox.kind).wrap(
            &inv.sandbox,
            &CommandSpec {
                program: self.command.to_string_lossy().into_owned(),
                args: argv(self.kind, &inv.prompt, inv.model.as_deref()),
                env: vec![
                    (
                        "PIZZA_DELIVERY_HANDOFF".to_owned(),
                        inv.handoff_path.to_string_lossy().into_owned(),
                    ),
                    (
                        "PIZZA_DELIVERY_EGRESS_LOG".to_owned(),
                        inv.egress_log.to_string_lossy().into_owned(),
                    ),
                    (
                        "PIZZA_DELIVERY_EGRESS_ALLOW".to_owned(),
                        inv.sandbox.egress_allowlist.join(","),
                    ),
                ],
            },
        );
        let mut child = Command::new(&launch.program)
            .args(&launch.args)
            .envs(launch.env.iter().map(|(k, v)| (k, v)))
            .current_dir(&inv.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .with_context(|| format!("spawn {}", self.command.display()))?;
        let pid = child.id();
        on_spawn(pid);
        let stdout_reader = drain(child.stdout.take().context("agent stdout")?);
        let stderr_reader = drain(child.stderr.take().context("agent stderr")?);
        let started = Instant::now();
        let mut timed_out = false;
        let mut exit_code = None;
        loop {
            if let Some(status) = child.try_wait().context("wait for agent")? {
                exit_code = status.code();
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
        Ok(AgentOutput {
            stdout: stdout_reader.join().unwrap_or_default(),
            stderr: stderr_reader.join().unwrap_or_default(),
            exit_code,
            timed_out,
        })
    }
}

pub struct Fleet {
    entries: BTreeMap<String, (AdapterKind, Box<dyn Agent>)>,
}

impl Fleet {
    pub fn new(entries: Vec<(String, AdapterKind, Box<dyn Agent>)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(name, kind, agent)| (name, (kind, agent)))
                .collect(),
        }
    }

    pub fn from_routing(routing: &RoutingConfig) -> Self {
        Self::new(
            routing
                .agents
                .iter()
                .map(|(name, def)| {
                    (
                        name.clone(),
                        def.adapter,
                        Box::new(CliAgent::new(def.adapter, &def.command)) as Box<dyn Agent>,
                    )
                })
                .collect(),
        )
    }

    pub fn get(&self, name: &str) -> Option<(AdapterKind, &dyn Agent)> {
        self.entries
            .get(name)
            .map(|(kind, agent)| (*kind, agent.as_ref()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_cli_gets_its_own_flags_and_the_model_when_one_is_chosen() {
        assert_eq!(
            argv(AdapterKind::Claude, "hi", Some("opus")),
            [
                "-p",
                "hi",
                "--output-format",
                "stream-json",
                "--verbose",
                "--model",
                "opus"
            ]
        );
        assert_eq!(
            argv(AdapterKind::Opencode, "hi", Some("a/b")),
            ["run", "--format", "json", "-m", "a/b", "hi"]
        );
        assert_eq!(
            argv(AdapterKind::Hermes, "hi", None),
            ["chat", "-q", "hi", "-Q", "--format", "stream-json"]
        );
        assert_eq!(
            argv(AdapterKind::Agy, "hi", None),
            ["-p", "hi", "--output-format", "json"]
        );
    }
}
