use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxKind {
    #[default]
    None,
    Bubblewrap,
    SystemdRun,
}

impl SandboxKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxKind::None => "none",
            SandboxKind::Bubblewrap => "bubblewrap",
            SandboxKind::SystemdRun => "systemd-run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub path: String,
    pub writable: bool,
}

impl Mount {
    pub fn describe(&self) -> String {
        format!("{}:{}", self.path, if self.writable { "rw" } else { "ro" })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProfile {
    pub name: String,
    pub kind: SandboxKind,
    pub mounts: Vec<Mount>,
    pub egress_allowlist: Vec<String>,
    pub memory_max: Option<String>,
}

impl SandboxProfile {
    pub fn hash(&self) -> String {
        let canonical = serde_json::to_vec(self).unwrap_or_default();
        format!("{:x}", Sha256::digest(canonical))
    }
}

impl Default for SandboxProfile {
    fn default() -> Self {
        TrustConfig::default().profile_for("", &[])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

pub trait Sandbox: Send + Sync {
    fn wrap(&self, profile: &SandboxProfile, spec: &CommandSpec) -> CommandSpec;
}

pub struct NoSandbox;
pub struct Bubblewrap;
pub struct SystemdRun;

impl Sandbox for NoSandbox {
    fn wrap(&self, _profile: &SandboxProfile, spec: &CommandSpec) -> CommandSpec {
        spec.clone()
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

impl Sandbox for Bubblewrap {
    fn wrap(&self, profile: &SandboxProfile, spec: &CommandSpec) -> CommandSpec {
        let mut args = strings(&["--die-with-parent", "--dev-bind", "/dev", "/dev"]);
        args.extend(strings(&["--proc", "/proc"]));
        for mount in &profile.mounts {
            let flag = if mount.writable {
                "--bind"
            } else {
                "--ro-bind"
            };
            args.extend([flag.to_owned(), mount.path.clone(), mount.path.clone()]);
        }
        args.push("--".to_owned());
        args.push(spec.program.clone());
        args.extend(spec.args.iter().cloned());
        CommandSpec {
            program: "bwrap".to_owned(),
            args,
            env: spec.env.clone(),
        }
    }
}

impl Sandbox for SystemdRun {
    fn wrap(&self, profile: &SandboxProfile, spec: &CommandSpec) -> CommandSpec {
        let mut args = strings(&[
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--property=NoNewPrivileges=yes",
        ]);
        if let Some(max) = &profile.memory_max {
            args.push(format!("--property=MemoryMax={max}"));
        }
        args.push("--".to_owned());
        args.push(spec.program.clone());
        args.extend(spec.args.iter().cloned());
        CommandSpec {
            program: "systemd-run".to_owned(),
            args,
            env: spec.env.clone(),
        }
    }
}

pub fn sandbox_for(kind: SandboxKind) -> &'static dyn Sandbox {
    match kind {
        SandboxKind::None => &NoSandbox,
        SandboxKind::Bubblewrap => &Bubblewrap,
        SandboxKind::SystemdRun => &SystemdRun,
    }
}

pub fn host_allowed(allowlist: &[String], host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    allowlist.iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        match pattern.strip_prefix("*.") {
            Some(suffix) => host.ends_with(&format!(".{suffix}")),
            None => host == pattern,
        }
    })
}

pub fn blocked_hosts(allowlist: &[String], requested_log: &str) -> Vec<String> {
    let mut blocked: Vec<String> = Vec::new();
    for host in requested_log
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
    {
        if !host_allowed(allowlist, host) && !blocked.iter().any(|b| b == host) {
            blocked.push(host.to_owned());
        }
    }
    blocked
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeCheck {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

fn check(name: &str, program: &str, args: &[&str]) -> SmokeCheck {
    SmokeCheck {
        name: name.to_owned(),
        program: program.to_owned(),
        args: strings(args),
    }
}

pub fn required_checks(agent_command: &str) -> Vec<SmokeCheck> {
    vec![
        check("git", "git", &["--version"]),
        check("gh", "gh", &["--version"]),
        check("nix", "nix", &["--version"]),
        check("devenv", "devenv", &["version"]),
        check("cargo", "cargo", &["--version"]),
        check("agent", agent_command, &["--version"]),
        check("engram", "engram", &["version"]),
        check("mnemon", "mnemon", &["--version"]),
        check(
            "model-api-network",
            "curl",
            &[
                "-sS",
                "-I",
                "-o",
                "/dev/null",
                "--max-time",
                "10",
                "https://api.anthropic.com",
            ],
        ),
    ]
}

pub trait Probe {
    fn succeeds(&self, spec: &CommandSpec) -> bool;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeReport {
    pub failed: Vec<String>,
}

impl SmokeReport {
    pub fn passed(&self) -> bool {
        self.failed.is_empty()
    }
}

pub fn smoke_check(
    sandbox: &dyn Sandbox,
    profile: &SandboxProfile,
    checks: &[SmokeCheck],
    probe: &dyn Probe,
) -> SmokeReport {
    let failed = checks
        .iter()
        .filter(|c| {
            let spec = CommandSpec {
                program: c.program.clone(),
                args: c.args.clone(),
                env: Vec::new(),
            };
            !probe.succeeds(&sandbox.wrap(profile, &spec))
        })
        .map(|c| c.name.clone())
        .collect();
    SmokeReport { failed }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct RepoTrust {
    pub allowed_hosts: Vec<String>,
    pub protected_paths: Vec<String>,
    pub push_remote: Option<String>,
    pub sandbox: Option<SandboxKind>,
    pub memory_max: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(default)]
pub struct TrustConfig {
    pub default_sandbox: SandboxKind,
    pub default_hosts: Vec<String>,
    pub repos: BTreeMap<String, RepoTrust>,
}

#[derive(Debug, Error)]
#[error("invalid trust config: {0}")]
pub struct TrustError(String);

impl TrustConfig {
    pub fn from_json(text: &str) -> Result<Self, TrustError> {
        serde_json::from_str(text).map_err(|e| TrustError(e.to_string()))
    }

    pub fn profile_for(&self, repo: &str, writable_paths: &[String]) -> SandboxProfile {
        let trust = self.repos.get(repo);
        let kind = trust
            .and_then(|t| t.sandbox)
            .unwrap_or(self.default_sandbox);
        let mut egress_allowlist = self.default_hosts.clone();
        if let Some(t) = trust {
            egress_allowlist.extend(t.allowed_hosts.iter().cloned());
        }
        let mut mounts = vec![Mount {
            path: "/".to_owned(),
            writable: false,
        }];
        mounts.extend(writable_paths.iter().map(|p| Mount {
            path: p.clone(),
            writable: true,
        }));
        SandboxProfile {
            name: kind.as_str().to_owned(),
            kind,
            mounts,
            egress_allowlist,
            memory_max: trust.and_then(|t| t.memory_max.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn profile(kind: SandboxKind) -> SandboxProfile {
        SandboxProfile {
            name: "p".into(),
            kind,
            mounts: vec![
                Mount {
                    path: "/".into(),
                    writable: false,
                },
                Mount {
                    path: "/work".into(),
                    writable: true,
                },
            ],
            egress_allowlist: vec!["api.anthropic.com".into(), "*.github.com".into()],
            memory_max: Some("2G".into()),
        }
    }

    fn spec() -> CommandSpec {
        CommandSpec {
            program: "claude".into(),
            args: vec!["-p".into(), "hi".into()],
            env: vec![("K".into(), "V".into())],
        }
    }

    #[test]
    fn bubblewrap_binds_each_mount_and_runs_the_command_after_the_separator() {
        let wrapped = Bubblewrap.wrap(&profile(SandboxKind::Bubblewrap), &spec());
        assert_eq!(wrapped.program, "bwrap");
        let joined = wrapped.args.join(" ");
        assert!(joined.contains("--ro-bind / /"));
        assert!(joined.contains("--bind /work /work"));
        assert!(joined.ends_with("-- claude -p hi"));
        assert_eq!(wrapped.env, spec().env);
    }

    #[test]
    fn systemd_run_scopes_the_command_with_resource_limits() {
        let wrapped = SystemdRun.wrap(&profile(SandboxKind::SystemdRun), &spec());
        assert_eq!(wrapped.program, "systemd-run");
        let joined = wrapped.args.join(" ");
        assert!(joined.contains("--scope"));
        assert!(joined.contains("--property=MemoryMax=2G"));
        assert!(joined.ends_with("-- claude -p hi"));
    }

    #[test]
    fn no_sandbox_leaves_the_command_alone() {
        assert_eq!(NoSandbox.wrap(&profile(SandboxKind::None), &spec()), spec());
    }

    #[test]
    fn profile_hash_is_stable_and_sensitive_to_the_allowlist() {
        let a = profile(SandboxKind::Bubblewrap);
        let mut b = a.clone();
        assert_eq!(a.hash(), b.hash());
        b.egress_allowlist.push("evil.example".into());
        assert_ne!(a.hash(), b.hash());
    }

    #[test]
    fn allowlist_matches_exact_hosts_and_wildcard_subdomains_only() {
        let list = profile(SandboxKind::None).egress_allowlist;
        assert!(host_allowed(&list, "api.anthropic.com"));
        assert!(host_allowed(&list, "API.github.com"));
        assert!(!host_allowed(&list, "github.com"));
        assert!(!host_allowed(&list, "evilgithub.com"));
        assert!(!host_allowed(&list, "anthropic.com"));
    }

    #[test]
    fn blocked_hosts_are_the_distinct_requests_outside_the_allowlist() {
        let list = profile(SandboxKind::None).egress_allowlist;
        let log = "api.anthropic.com\nevil.example\n\nevil.example\nx.github.com\ntracker.io\n";
        assert_eq!(
            blocked_hosts(&list, log),
            vec!["evil.example", "tracker.io"]
        );
    }

    struct FakeProbe {
        broken: &'static str,
        seen: RefCell<Vec<String>>,
    }

    impl Probe for FakeProbe {
        fn succeeds(&self, spec: &CommandSpec) -> bool {
            self.seen.borrow_mut().push(spec.args.join(" "));
            !spec.args.iter().any(|a| a == self.broken)
        }
    }

    #[test]
    fn smoke_check_fails_a_profile_that_breaks_a_required_tool_and_names_it() {
        let probe = FakeProbe {
            broken: "devenv",
            seen: RefCell::new(Vec::new()),
        };
        let report = smoke_check(
            &Bubblewrap,
            &profile(SandboxKind::Bubblewrap),
            &required_checks("claude"),
            &probe,
        );
        assert!(!report.passed());
        assert_eq!(report.failed, vec!["devenv"]);
        assert!(probe
            .seen
            .borrow()
            .iter()
            .all(|a| a.contains("--die-with-parent")));
    }

    #[test]
    fn smoke_check_passes_when_every_tool_works() {
        let probe = FakeProbe {
            broken: "none-such",
            seen: RefCell::new(Vec::new()),
        };
        let report = smoke_check(
            &NoSandbox,
            &profile(SandboxKind::None),
            &required_checks("claude"),
            &probe,
        );
        assert!(report.passed());
    }

    #[test]
    fn required_checks_cover_the_tools_the_pipeline_needs() {
        let names: Vec<String> = required_checks("claude")
            .into_iter()
            .map(|c| c.name)
            .collect();
        for tool in [
            "git",
            "gh",
            "nix",
            "devenv",
            "cargo",
            "agent",
            "engram",
            "mnemon",
            "model-api-network",
        ] {
            assert!(names.iter().any(|n| n == tool), "missing {tool}");
        }
    }

    #[test]
    fn trust_config_resolves_a_per_repo_profile_over_the_defaults() {
        let trust = TrustConfig::from_json(
            r#"{"default_sandbox":"bubblewrap","default_hosts":["api.anthropic.com"],
                "repos":{"/r":{"allowed_hosts":["crates.io"],"sandbox":"systemd-run","memory_max":"4G"}}}"#,
        )
        .unwrap_or_default();
        let known = trust.profile_for("/r", &["/w".to_owned()]);
        let other = trust.profile_for("/other", &[]);
        assert_eq!(known.kind, SandboxKind::SystemdRun);
        assert_eq!(
            known.egress_allowlist,
            vec!["api.anthropic.com", "crates.io"]
        );
        assert_eq!(known.memory_max.as_deref(), Some("4G"));
        assert!(known.mounts.contains(&Mount {
            path: "/w".into(),
            writable: true
        }));
        assert_eq!(other.kind, SandboxKind::Bubblewrap);
        assert_eq!(other.egress_allowlist, vec!["api.anthropic.com"]);
    }

    #[test]
    fn malformed_trust_config_is_an_error() {
        assert!(TrustConfig::from_json("{nope").is_err());
    }
}
