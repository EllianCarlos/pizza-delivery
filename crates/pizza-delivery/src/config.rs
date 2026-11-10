use std::path::PathBuf;

use pizza_delivery_core::budget::{Defaults, DEFAULT_CAP_TOKENS};
use pizza_delivery_core::routing::{ConfigError as RoutingError, RoutingConfig};
use pizza_delivery_core::{Caps, DEFAULT_RETENTION_MS};
use thiserror::Error;

pub const DEFAULT_HTTP_PORT: u16 = 7878;

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
    pub http_port: u16,
    pub notify_command: Option<PathBuf>,
    pub notify_opener: PathBuf,
    pub notify_muted: String,
    pub web_url: String,
    pub remote: Option<RemoteConfig>,
    pub reject_window_ms: u64,
    pub vapid_subject: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub audience: String,
    pub issuer: String,
    pub certs_url: String,
    pub emails: Vec<String>,
    pub submits_enabled: bool,
    pub submit_limit_per_minute: u32,
    pub refresh_min_ms: u64,
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
    #[error("unsafe remote access configuration: {0}")]
    UnsafeRemote(String),
    #[error("cannot read routing config {path}: {reason}")]
    RoutingUnreadable { path: PathBuf, reason: String },
    #[error("routing config {path}: {source}")]
    Routing { path: PathBuf, source: RoutingError },
}

fn remote_config(
    get: &impl Fn(&str) -> Option<String>,
) -> Result<Option<RemoteConfig>, ConfigError> {
    let on = |v: String| matches!(v.to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes");
    if !get("PIZZA_DELIVERY_REMOTE").is_some_and(on) {
        return Ok(None);
    }
    let unsafe_config = |what: &str| ConfigError::UnsafeRemote(what.to_owned());
    let audience = get("PIZZA_DELIVERY_ACCESS_AUD")
        .ok_or_else(|| unsafe_config("PIZZA_DELIVERY_ACCESS_AUD is missing"))?;
    let issuer = get("PIZZA_DELIVERY_ACCESS_ISSUER")
        .map(|i| i.trim_end_matches('/').to_owned())
        .ok_or_else(|| unsafe_config("PIZZA_DELIVERY_ACCESS_ISSUER is missing"))?;
    let emails: Vec<String> = get("PIZZA_DELIVERY_ACCESS_EMAILS")
        .unwrap_or_default()
        .split(',')
        .map(|e| e.trim().to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    if emails.is_empty() {
        return Err(unsafe_config("the email allowlist is empty"));
    }
    Ok(Some(RemoteConfig {
        certs_url: get("PIZZA_DELIVERY_ACCESS_CERTS_URL")
            .unwrap_or_else(|| format!("{issuer}/cdn-cgi/access/certs")),
        audience,
        issuer,
        emails,
        submits_enabled: get("PIZZA_DELIVERY_REMOTE_SUBMITS").map_or(true, |v| {
            !matches!(v.to_ascii_lowercase().as_str(), "off" | "0" | "false")
        }),
        submit_limit_per_minute: get("PIZZA_DELIVERY_REMOTE_SUBMIT_LIMIT")
            .and_then(|v| v.parse().ok())
            .unwrap_or(10),
        refresh_min_ms: get("PIZZA_DELIVERY_ACCESS_REFRESH_MIN_MS")
            .and_then(|v| v.parse().ok())
            .unwrap_or(5_000),
    }))
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
        let remote = remote_config(&get)?;
        Ok(Self {
            remote,
            reject_window_ms: number("PIZZA_DELIVERY_AUTH_REJECT_WINDOW_MS", 60_000),
            vapid_subject: get("PIZZA_DELIVERY_VAPID_SUBJECT")
                .unwrap_or_else(|| "mailto:owner@pizza-delivery.invalid".to_owned()),
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
            http_port: get("PIZZA_DELIVERY_HTTP_PORT")
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_HTTP_PORT),
            notify_command: match get("PIZZA_DELIVERY_NOTIFY").as_deref() {
                Some("off") => None,
                Some(command) => Some(PathBuf::from(command)),
                None => Some(PathBuf::from("notify-send")),
            },
            notify_opener: path_or("PIZZA_DELIVERY_OPENER", PathBuf::from("xdg-open")),
            notify_muted: get("PIZZA_DELIVERY_NOTIFY_MUTE").unwrap_or_default(),
            web_url: get("PIZZA_DELIVERY_WEB_URL")
                .unwrap_or_else(|| "http://127.0.0.1:7878".to_owned()),
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

    #[test]
    fn notifications_default_to_notify_send_and_can_be_switched_off() {
        let defaults = Config::from_lookup(lookup(&[("HOME", "/h")]));
        let off = Config::from_lookup(lookup(&[("HOME", "/h"), ("PIZZA_DELIVERY_NOTIFY", "off")]));
        assert_eq!(
            defaults.ok().and_then(|c| c.notify_command),
            Some(PathBuf::from("notify-send"))
        );
        assert_eq!(off.ok().and_then(|c| c.notify_command), None);
    }

    fn remote(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut pairs = vec![("HOME", "/h"), ("PIZZA_DELIVERY_REMOTE", "on")];
        pairs.extend_from_slice(extra);
        Config::from_lookup(lookup(&pairs))
    }

    #[test]
    fn remote_access_is_off_by_default() {
        let cfg = Config::from_lookup(lookup(&[("HOME", "/h")]));
        assert_eq!(cfg.ok().map(|c| c.remote.is_none()), Some(true));
    }

    #[test]
    fn remote_access_needs_an_audience_issuer_and_a_non_empty_allowlist() {
        let full = [
            ("PIZZA_DELIVERY_ACCESS_AUD", "aud1"),
            (
                "PIZZA_DELIVERY_ACCESS_ISSUER",
                "https://team.cloudflareaccess.com",
            ),
            (
                "PIZZA_DELIVERY_ACCESS_EMAILS",
                "Me@Example.com, you@example.com",
            ),
        ];
        let cfg = remote(&full).ok().and_then(|c| c.remote);
        assert_eq!(
            cfg.as_ref().map(|r| r.emails.clone()),
            Some(vec![
                "me@example.com".to_owned(),
                "you@example.com".to_owned()
            ])
        );
        assert_eq!(
            cfg.map(|r| r.certs_url),
            Some("https://team.cloudflareaccess.com/cdn-cgi/access/certs".to_owned())
        );
        for skip in 0..full.len() {
            let partial: Vec<_> = full
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, p)| *p)
                .collect();
            assert!(
                matches!(remote(&partial), Err(ConfigError::UnsafeRemote(_))),
                "missing {}",
                full[skip].0
            );
        }
        let blank = [full[0], full[1], ("PIZZA_DELIVERY_ACCESS_EMAILS", " , ")];
        assert!(matches!(remote(&blank), Err(ConfigError::UnsafeRemote(_))));
    }
}
