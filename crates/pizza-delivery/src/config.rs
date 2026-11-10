use std::path::PathBuf;

use pizza_delivery_core::budget::{Defaults, DEFAULT_CAP_TOKENS};
use pizza_delivery_core::routing::{ConfigError as RoutingError, RoutingConfig};
use pizza_delivery_core::{Caps, DEFAULT_RETENTION_MS};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub agent_command: PathBuf,
    pub trust_path: PathBuf,
    pub lease_ttl_ms: i64,
    pub stage_timeout_secs: Option<u64>,
    pub kill_grace_ms: u64,
    pub shutdown_grace_ms: u64,
    pub retention_ms: i64,
    pub caps: Caps,
    pub routing: RoutingConfig,
    pub routing_file: Option<RoutingFile>,
    pub projects_dir: PathBuf,
    pub engram_db: PathBuf,
    pub engram_command: PathBuf,
    pub mnemon_command: PathBuf,
    pub budget_cap_tokens: u64,
    pub budget_defaults: Defaults,
    pub mcp_catalog: PathBuf,
    pub mcp_secrets: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingFile {
    pub path: PathBuf,
    pub required: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot determine a data directory: set PIZZA_DELIVERY_DATA_DIR or HOME")]
    NoDataDir,
    #[error("cannot read routing config {path}: {reason}")]
    RoutingUnreadable { path: PathBuf, reason: String },
    #[error("routing config {path}: {source}")]
    Routing { path: PathBuf, source: RoutingError },
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let data_dir = get("PIZZA_DELIVERY_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| get("XDG_DATA_HOME").map(|d| PathBuf::from(d).join("pizza-delivery")))
            .or_else(|| get("HOME").map(|h| PathBuf::from(h).join(".local/share/pizza-delivery")))
            .ok_or(ConfigError::NoDataDir)?;
        let socket_path = get("PIZZA_DELIVERY_SOCKET")
            .map(PathBuf::from)
            .or_else(|| {
                get("XDG_RUNTIME_DIR").map(|d| PathBuf::from(d).join("pizza-delivery.sock"))
            })
            .unwrap_or_else(|| data_dir.join("pizza-delivery.sock"));
        let agent_command = get("PIZZA_DELIVERY_AGENT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("claude"));
        let home = get("HOME").map(PathBuf::from).unwrap_or_default();
        let path_or = |key: &str, default: PathBuf| get(key).map(PathBuf::from).unwrap_or(default);
        let trust_path = get("PIZZA_DELIVERY_TRUST")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("trust.json"));
        let mcp_catalog = get("PIZZA_DELIVERY_MCP_CATALOG")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("mcp-catalog.toml"));
        let mcp_secrets = get("PIZZA_DELIVERY_SECRETS")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("secrets"));
        let number =
            |key: &str, default: u64| get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
        let routing_file = get("PIZZA_DELIVERY_ROUTING")
            .map(|p| RoutingFile {
                path: PathBuf::from(p),
                required: true,
            })
            .or_else(|| {
                get("XDG_CONFIG_HOME")
                    .map(PathBuf::from)
                    .or_else(|| get("HOME").map(|h| PathBuf::from(h).join(".config")))
                    .map(|c| RoutingFile {
                        path: c.join("pizza-delivery").join("routing.toml"),
                        required: false,
                    })
            });
        Ok(Self {
            routing: RoutingConfig::builtin(&agent_command.to_string_lossy()),
            routing_file,
            data_dir,
            socket_path,
            agent_command,
            trust_path,
            lease_ttl_ms: number("PIZZA_DELIVERY_LEASE_TTL_MS", 30_000) as i64,
            stage_timeout_secs: get("PIZZA_DELIVERY_STAGE_TIMEOUT_SECS")
                .and_then(|v| v.parse().ok()),
            kill_grace_ms: number("PIZZA_DELIVERY_KILL_GRACE_MS", 5_000),
            shutdown_grace_ms: number("PIZZA_DELIVERY_SHUTDOWN_GRACE_MS", 30_000),
            retention_ms: number("PIZZA_DELIVERY_RETENTION_MS", DEFAULT_RETENTION_MS as u64) as i64,
            caps: Caps {
                global: number("PIZZA_DELIVERY_MAX_RUNNING", 2) as usize,
                per_agent: number("PIZZA_DELIVERY_MAX_PER_AGENT", 2) as usize,
                per_repo: number("PIZZA_DELIVERY_MAX_PER_REPO", 2) as usize,
            },
            projects_dir: path_or("PIZZA_DELIVERY_PROJECTS_DIR", home.join("Projects")),
            engram_db: path_or("PIZZA_DELIVERY_ENGRAM_DB", home.join(".engram/engram.db")),
            engram_command: path_or("PIZZA_DELIVERY_ENGRAM", PathBuf::from("engram")),
            mnemon_command: path_or("PIZZA_DELIVERY_MNEMON", PathBuf::from("mnemon")),
            mcp_catalog,
            mcp_secrets,
            budget_cap_tokens: number("PIZZA_DELIVERY_BUDGET_CAP_TOKENS", DEFAULT_CAP_TOKENS),
            budget_defaults: {
                let std = Defaults::standard();
                let tier = |name: &str, base: (u64, u64)| {
                    (
                        number(&format!("PIZZA_DELIVERY_ESTIMATE_{name}_TOKENS"), base.0),
                        number(&format!("PIZZA_DELIVERY_ESTIMATE_{name}_WALL_MS"), base.1),
                    )
                };
                Defaults {
                    small: tier("SMALL", std.small),
                    medium: tier("MEDIUM", std.medium),
                    large: tier("LARGE", std.large),
                }
            },
        })
    }

    pub fn load_routing(self) -> Result<Self, ConfigError> {
        let Some(file) = self.routing_file.clone() else {
            return Ok(self);
        };
        let text = match std::fs::read_to_string(&file.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !file.required => {
                return Ok(self)
            }
            Err(e) => {
                return Err(ConfigError::RoutingUnreadable {
                    path: file.path,
                    reason: e.to_string(),
                })
            }
        };
        let routing =
            self.routing
                .clone()
                .with_owner(&text)
                .map_err(|source| ConfigError::Routing {
                    path: file.path,
                    source,
                })?;
        Ok(Self { routing, ..self })
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("ledger.sqlite3")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    #[test]
    fn explicit_env_wins() {
        let cfg = Config::from_lookup(lookup(&[
            ("PIZZA_DELIVERY_DATA_DIR", "/d"),
            ("PIZZA_DELIVERY_SOCKET", "/s.sock"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(
            cfg.map(|c| (c.data_dir, c.socket_path)).ok(),
            Some(("/d".into(), "/s.sock".into()))
        );
    }

    #[test]
    fn xdg_defaults_apply() {
        let cfg = Config::from_lookup(lookup(&[
            ("XDG_DATA_HOME", "/x/data"),
            ("XDG_RUNTIME_DIR", "/run/user/1"),
        ]));
        assert_eq!(
            cfg.map(|c| (c.data_dir, c.socket_path)).ok(),
            Some((
                "/x/data/pizza-delivery".into(),
                "/run/user/1/pizza-delivery.sock".into()
            ))
        );
    }

    #[test]
    fn home_fallback_and_socket_in_data_dir() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/home/u")]));
        assert_eq!(
            cfg.map(|c| (c.data_dir, c.socket_path)).ok(),
            Some((
                "/home/u/.local/share/pizza-delivery".into(),
                "/home/u/.local/share/pizza-delivery/pizza-delivery.sock".into()
            ))
        );
    }

    #[test]
    fn agent_defaults_to_claude_and_limits_are_overridable() {
        let defaults = Config::from_lookup(lookup(&[("HOME", "/h")]));
        let custom = Config::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("PIZZA_DELIVERY_AGENT", "/bin/fake"),
            ("PIZZA_DELIVERY_LEASE_TTL_MS", "500"),
        ]));
        assert_eq!(
            defaults.map(|c| (c.agent_command, c.lease_ttl_ms)).ok(),
            Some(("claude".into(), 30_000))
        );
        assert_eq!(
            custom.map(|c| (c.agent_command, c.lease_ttl_ms)).ok(),
            Some(("/bin/fake".into(), 500))
        );
    }

    #[test]
    fn an_explicit_routing_file_must_exist_and_be_valid() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::TempDir::new()?;
        let good = dir.path().join("good.toml");
        let bad = dir.path().join("bad.toml");
        std::fs::write(&good, "quota_threshold = 0.5\n")?;
        std::fs::write(&bad, "[[rules]]\nchain = [{ agent = \"ghost\" }]\n")?;
        let load = |path: &std::path::Path| {
            Config::from_lookup(lookup(&[
                ("HOME", "/h"),
                ("PIZZA_DELIVERY_ROUTING", &path.to_string_lossy()),
            ]))
            .and_then(Config::load_routing)
        };
        assert_eq!(load(&good)?.routing.threshold_permille, 500);
        assert!(matches!(load(&bad), Err(ConfigError::Routing { .. })));
        assert!(matches!(
            load(&dir.path().join("absent.toml")),
            Err(ConfigError::RoutingUnreadable { .. })
        ));
        Ok(())
    }

    #[test]
    fn the_default_routing_file_is_optional() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let cfg = Config::from_lookup(lookup(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("HOME", "/h"),
        ]))?
        .load_routing()?;
        assert_eq!(cfg.routing, RoutingConfig::builtin("claude"));
        Ok(())
    }

    #[test]
    fn resilience_limits_have_design_defaults_and_are_overridable() {
        let defaults = Config::from_lookup(lookup(&[("HOME", "/h")]));
        let custom = Config::from_lookup(lookup(&[
            ("HOME", "/h"),
            ("PIZZA_DELIVERY_STAGE_TIMEOUT_SECS", "7"),
            ("PIZZA_DELIVERY_MAX_RUNNING", "5"),
            ("PIZZA_DELIVERY_MAX_PER_REPO", "1"),
        ]));
        assert_eq!(
            defaults.map(|c| (c.stage_timeout_secs, c.caps)).ok(),
            Some((
                None,
                Caps {
                    global: 2,
                    per_agent: 2,
                    per_repo: 2
                }
            ))
        );
        assert_eq!(
            custom.map(|c| (c.stage_timeout_secs, c.caps)).ok(),
            Some((
                Some(7),
                Caps {
                    global: 5,
                    per_agent: 2,
                    per_repo: 1
                }
            ))
        );
    }

    #[test]
    fn nothing_to_go_on_is_an_error() {
        assert!(Config::from_lookup(lookup(&[])).is_err());
    }
}
