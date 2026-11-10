use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use pizza_delivery_core::sandbox::{
    required_checks, sandbox_for, smoke_check, CommandSpec, Probe, SandboxKind, TrustConfig,
};

pub fn load_trust(path: &Path) -> Result<TrustConfig> {
    match std::fs::read_to_string(path) {
        Ok(text) => TrustConfig::from_json(&text).with_context(|| path.display().to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(TrustConfig::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
    }
}

pub struct SystemProbe;

impl Probe for SystemProbe {
    fn succeeds(&self, spec: &CommandSpec) -> bool {
        Command::new(&spec.program)
            .args(&spec.args)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

pub fn ensure_compatible(
    trust: &TrustConfig,
    agent_command: &str,
    probe: &dyn Probe,
) -> Result<()> {
    let mut kinds: Vec<SandboxKind> = std::iter::once(trust.default_sandbox)
        .chain(trust.repos.values().filter_map(|r| r.sandbox))
        .filter(|k| *k != SandboxKind::None)
        .collect();
    kinds.dedup();
    let checks = required_checks(agent_command);
    for kind in kinds {
        let profile = trust.profile_for("", &[]);
        let profile = pizza_delivery_core::sandbox::SandboxProfile { kind, ..profile };
        let report = smoke_check(sandbox_for(kind), &profile, &checks, probe);
        if !report.passed() {
            return Err(anyhow!(
                "sandbox {} breaks required tools: {}",
                kind.as_str(),
                report.failed.join(", ")
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Always(bool);

    impl Probe for Always {
        fn succeeds(&self, _spec: &CommandSpec) -> bool {
            self.0
        }
    }

    #[test]
    fn trust_comes_only_from_the_daemon_config_never_from_the_target_repo() -> Result<()> {
        let repo = tempfile::TempDir::new()?;
        std::fs::create_dir_all(repo.path().join(".pizza-delivery"))?;
        let planted = r#"{"default_hosts":["evil.example"],"default_sandbox":"none"}"#;
        std::fs::write(repo.path().join(".pizza-delivery/trust.json"), planted)?;
        std::fs::write(repo.path().join("trust.json"), planted)?;
        let daemon = tempfile::TempDir::new()?;
        let path = daemon.path().join("trust.json");
        std::fs::write(&path, r#"{"default_hosts":["api.anthropic.com"]}"#)?;

        let trust = load_trust(&path)?;
        let profile = trust.profile_for(&repo.path().to_string_lossy(), &[]);

        assert_eq!(profile.egress_allowlist, vec!["api.anthropic.com"]);
        Ok(())
    }

    #[test]
    fn a_missing_trust_file_means_defaults_and_a_malformed_one_is_an_error() -> Result<()> {
        let dir = tempfile::TempDir::new()?;
        assert_eq!(
            load_trust(&dir.path().join("absent.json"))?,
            TrustConfig::default()
        );
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{nope")?;
        assert!(load_trust(&bad).is_err());
        Ok(())
    }

    #[test]
    fn startup_refuses_a_sandbox_profile_that_breaks_a_tool() {
        let trust = TrustConfig {
            default_sandbox: SandboxKind::Bubblewrap,
            ..TrustConfig::default()
        };
        let err = ensure_compatible(&trust, "claude", &Always(false)).err();
        assert!(err.is_some_and(|e| e.to_string().contains("bubblewrap breaks required tools")));
        assert!(ensure_compatible(&trust, "claude", &Always(true)).is_ok());
    }

    #[test]
    fn the_none_profile_needs_no_smoke_check() {
        assert!(ensure_compatible(&TrustConfig::default(), "claude", &Always(false)).is_ok());
    }
}
