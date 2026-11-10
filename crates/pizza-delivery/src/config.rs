use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub data_dir: PathBuf,
    pub socket_path: PathBuf,
    pub agent_command: PathBuf,
    pub trust_path: PathBuf,
    pub lease_ttl_ms: i64,
    pub stage_timeout_secs: u64,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot determine a data directory: set PIZZA_DELIVERY_DATA_DIR or HOME")]
    NoDataDir,
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
        let trust_path = get("PIZZA_DELIVERY_TRUST")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("trust.json"));
        let number =
            |key: &str, default: u64| get(key).and_then(|v| v.parse().ok()).unwrap_or(default);
        Ok(Self {
            data_dir,
            socket_path,
            agent_command,
            trust_path,
            lease_ttl_ms: number("PIZZA_DELIVERY_LEASE_TTL_MS", 30_000) as i64,
            stage_timeout_secs: number("PIZZA_DELIVERY_STAGE_TIMEOUT_SECS", 3600),
        })
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
    fn nothing_to_go_on_is_an_error() {
        assert!(Config::from_lookup(lookup(&[])).is_err());
    }
}
