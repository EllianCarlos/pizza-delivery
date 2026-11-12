use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::event::EventPayload;
use crate::triage::Feedback;

pub const MAX_ACTIVE_PER_STAGE: usize = 3;
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 600;
pub const DEFAULT_MAX_LIFETIME_SECS: u64 = 3600;

fn default_idle() -> u64 {
    DEFAULT_IDLE_TIMEOUT_SECS
}

fn default_lifetime() -> u64 {
    DEFAULT_MAX_LIFETIME_SECS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub credentials: Vec<String>,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub egress: Vec<String>,
    #[serde(default = "default_idle")]
    pub idle_timeout_secs: u64,
    #[serde(default = "default_lifetime")]
    pub max_lifetime_secs: u64,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub auto: bool,
}

impl CatalogEntry {
    fn allows(&self, stage: &str, agent: &str) -> bool {
        let listed = |allowed: &[String], value: &str| {
            allowed.is_empty() || allowed.iter().any(|a| a == value)
        };
        listed(&self.stages, stage) && listed(&self.agents, agent)
    }
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog is not valid TOML: {0}")]
    Toml(String),
    #[error("catalog entry '{0}' needs a command or a url")]
    NoLaunch(String),
    #[error("catalog entry '{0}' is listed twice")]
    Duplicate(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct Catalog {
    #[serde(default, rename = "server")]
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Self, CatalogError> {
        let catalog: Catalog =
            toml::from_str(text).map_err(|e| CatalogError::Toml(e.to_string()))?;
        let mut seen = BTreeMap::new();
        for entry in &catalog.entries {
            if entry.command.is_empty() && entry.url.is_none() {
                return Err(CatalogError::NoLaunch(entry.name.clone()));
            }
            if seen.insert(entry.name.as_str(), ()).is_some() {
                return Err(CatalogError::Duplicate(entry.name.clone()));
            }
        }
        Ok(catalog)
    }

    pub fn get(&self, name: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|e| e.name == name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum McpEvent {
    Requested {
        server: String,
    },
    Activated {
        server: String,
        egress: Vec<String>,
        write: bool,
    },
    Denied {
        server: String,
        reason: String,
        feedback: Feedback,
    },
    Deactivated {
        server: String,
        reason: String,
    },
    Calls {
        server: String,
        count: u64,
    },
}

impl McpEvent {
    pub fn payload(&self) -> EventPayload {
        match self.clone() {
            McpEvent::Requested { server } => EventPayload::McpRequested { server },
            McpEvent::Activated {
                server,
                egress,
                write,
            } => EventPayload::McpActivated {
                server,
                egress,
                write,
            },
            McpEvent::Denied { server, reason, .. } => EventPayload::McpDenied { server, reason },
            McpEvent::Deactivated { server, reason } => {
                EventPayload::McpDeactivated { server, reason }
            }
            McpEvent::Calls { server, count } => EventPayload::McpCalls { server, count },
        }
    }

    pub fn feedback(&self) -> Option<&Feedback> {
        match self {
            McpEvent::Denied { feedback, .. } => Some(feedback),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activation {
    Granted(CatalogEntry),
    Already,
    Denied(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogIndexEntry {
    pub name: String,
    pub description: String,
    pub active: bool,
}

struct Live {
    since_ms: i64,
    last_used_ms: i64,
    calls: u64,
}

pub struct Broker {
    catalog: Catalog,
    stage: String,
    agent: String,
    live: BTreeMap<String, Live>,
    events: Vec<McpEvent>,
}

impl Broker {
    pub fn new(catalog: Catalog, stage: &str, agent: &str) -> Self {
        Self {
            catalog,
            stage: stage.to_owned(),
            agent: agent.to_owned(),
            live: BTreeMap::new(),
            events: Vec::new(),
        }
    }

    pub fn list(&self) -> Vec<CatalogIndexEntry> {
        self.catalog
            .entries
            .iter()
            .filter(|e| e.allows(&self.stage, &self.agent))
            .map(|e| CatalogIndexEntry {
                name: e.name.clone(),
                description: e.description.clone(),
                active: self.live.contains_key(&e.name),
            })
            .collect()
    }

    pub fn active(&self) -> Vec<String> {
        self.live.keys().cloned().collect()
    }

    pub fn activate(&mut self, name: &str, now_ms: i64) -> Activation {
        self.events.push(McpEvent::Requested {
            server: name.to_owned(),
        });
        if self.live.contains_key(name) {
            return Activation::Already;
        }
        match self.refusal(name) {
            Some(reason) => {
                self.deny(name, &reason);
                Activation::Denied(reason)
            }
            None => {
                let Some(entry) = self.catalog.get(name).cloned() else {
                    return Activation::Denied(format!("'{name}' is not in the catalog"));
                };
                self.live.insert(
                    name.to_owned(),
                    Live {
                        since_ms: now_ms,
                        last_used_ms: now_ms,
                        calls: 0,
                    },
                );
                self.events.push(McpEvent::Activated {
                    server: name.to_owned(),
                    egress: entry.egress.clone(),
                    write: entry.write,
                });
                Activation::Granted(entry)
            }
        }
    }

    fn refusal(&self, name: &str) -> Option<String> {
        let Some(entry) = self.catalog.get(name) else {
            return Some(format!("'{name}' is not in the catalog"));
        };
        if !entry.allows(&self.stage, &self.agent) {
            return Some(format!(
                "'{name}' is not allowed in stage '{}' for agent '{}'",
                self.stage, self.agent
            ));
        }
        if entry.write && entry.stages.is_empty() {
            return Some(format!(
                "'{name}' is write-capable and names no allowed stages"
            ));
        }
        if !entry.auto {
            return Some(format!("'{name}' needs human approval, it is not auto"));
        }
        if self.live.len() >= MAX_ACTIVE_PER_STAGE {
            return Some(format!(
                "at most {MAX_ACTIVE_PER_STAGE} servers may be active per stage"
            ));
        }
        None
    }

    fn deny(&mut self, name: &str, reason: &str) {
        self.events.push(McpEvent::Denied {
            server: name.to_owned(),
            reason: reason.to_owned(),
            feedback: Feedback {
                kind: "question".to_owned(),
                severity: "medium".to_owned(),
                text: format!(
                    "Agent requested MCP server '{name}' in stage '{}' and was denied: {reason}",
                    self.stage
                ),
                confidence: None,
                evidence: None,
                alternatives: Vec::new(),
                impact: None,
            },
        });
    }

    pub fn deactivate(&mut self, name: &str, reason: &str) -> bool {
        let Some(live) = self.live.remove(name) else {
            return false;
        };
        self.events.push(McpEvent::Calls {
            server: name.to_owned(),
            count: live.calls,
        });
        self.events.push(McpEvent::Deactivated {
            server: name.to_owned(),
            reason: reason.to_owned(),
        });
        true
    }

    pub fn record_call(&mut self, name: &str, now_ms: i64) -> bool {
        match self.live.get_mut(name) {
            Some(live) => {
                live.calls += 1;
                live.last_used_ms = now_ms;
                true
            }
            None => false,
        }
    }

    pub fn expire(&mut self, now_ms: i64) -> Vec<String> {
        let due: Vec<(String, &'static str)> = self
            .live
            .iter()
            .filter_map(|(name, live)| {
                let entry = self.catalog.get(name)?;
                let lifetime =
                    i64::try_from(entry.max_lifetime_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
                let idle =
                    i64::try_from(entry.idle_timeout_secs.saturating_mul(1000)).unwrap_or(i64::MAX);
                if now_ms - live.since_ms >= lifetime {
                    Some((name.clone(), "max-lifetime"))
                } else if now_ms - live.last_used_ms >= idle {
                    Some((name.clone(), "idle-timeout"))
                } else {
                    None
                }
            })
            .collect();
        due.into_iter()
            .map(|(name, reason)| {
                self.deactivate(&name, reason);
                name
            })
            .collect()
    }

    pub fn end_stage(&mut self) -> Vec<String> {
        let names = self.active();
        for name in &names {
            self.deactivate(name, "stage-end");
        }
        names
    }

    pub fn take_events(&mut self) -> Vec<McpEvent> {
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATALOG: &str = r#"
[[server]]
name = "docs"
description = "Library docs"
command = ["docs-mcp", "--stdio"]
credentials = ["DOCS_TOKEN"]
egress = ["docs.example.com"]
stages = ["implement"]
auto = true

[[server]]
name = "browser"
description = "Browse"
url = "https://browser.example.com/mcp"
auto = false

[[server]]
name = "db-write"
command = ["db"]
write = true
auto = true

[[server]]
name = "a"
command = ["a"]
auto = true
[[server]]
name = "b"
command = ["b"]
auto = true
[[server]]
name = "c"
command = ["c"]
auto = true
"#;

    fn broker(stage: &str) -> Broker {
        Broker::new(Catalog::parse(CATALOG).unwrap_or_default(), stage, "claude")
    }

    fn denial(events: Vec<McpEvent>) -> Option<(String, Feedback)> {
        events.into_iter().find_map(|e| match e {
            McpEvent::Denied {
                reason, feedback, ..
            } => Some((reason, feedback)),
            _ => None,
        })
    }

    #[test]
    fn an_auto_entry_activates_and_is_ledgered_with_its_egress_hosts() {
        let mut b = broker("implement");
        assert!(matches!(b.activate("docs", 0), Activation::Granted(_)));
        assert_eq!(
            b.take_events(),
            vec![
                McpEvent::Requested {
                    server: "docs".into()
                },
                McpEvent::Activated {
                    server: "docs".into(),
                    egress: vec!["docs.example.com".into()],
                    write: false
                },
            ]
        );
    }

    #[test]
    fn a_non_auto_request_is_denied_and_becomes_a_feedback_entry() {
        let mut b = broker("implement");
        assert!(matches!(b.activate("browser", 0), Activation::Denied(_)));
        let (reason, feedback) = denial(b.take_events()).unwrap_or_else(|| {
            (
                String::new(),
                Feedback {
                    kind: String::new(),
                    severity: String::new(),
                    text: String::new(),
                    confidence: None,
                    evidence: None,
                    alternatives: vec![],
                    impact: None,
                },
            )
        });
        assert!(reason.contains("not auto"));
        assert_eq!(feedback.kind, "question");
        assert!(feedback.text.contains("'browser'"));
    }

    #[test]
    fn an_unknown_request_is_denied_as_feedback() {
        let mut b = broker("implement");
        assert!(matches!(b.activate("nope", 0), Activation::Denied(_)));
        assert!(denial(b.take_events()).is_some());
    }

    #[test]
    fn an_entry_outside_its_allowed_stages_is_denied_and_hidden_from_the_list() {
        let mut b = broker("review");
        assert!(!b.list().iter().any(|e| e.name == "docs"));
        assert!(matches!(b.activate("docs", 0), Activation::Denied(_)));
    }

    #[test]
    fn a_write_capable_entry_without_stages_is_denied() {
        let mut b = broker("implement");
        assert!(matches!(b.activate("db-write", 0), Activation::Denied(_)));
    }

    #[test]
    fn at_most_three_servers_are_active_per_stage() {
        let mut b = broker("implement");
        for name in ["a", "b", "c"] {
            assert!(matches!(b.activate(name, 0), Activation::Granted(_)));
        }
        assert!(matches!(b.activate("docs", 0), Activation::Denied(_)));
        assert_eq!(b.active().len(), 3);
    }

    #[test]
    fn activating_twice_is_idempotent() {
        let mut b = broker("implement");
        b.activate("docs", 0);
        assert_eq!(b.activate("docs", 1), Activation::Already);
    }

    #[test]
    fn deactivation_ledgers_the_tool_call_count_and_reason() {
        let mut b = broker("implement");
        b.activate("docs", 0);
        b.record_call("docs", 5);
        b.record_call("docs", 6);
        b.take_events();
        assert!(b.deactivate("docs", "explicit"));
        assert_eq!(
            b.take_events(),
            vec![
                McpEvent::Calls {
                    server: "docs".into(),
                    count: 2
                },
                McpEvent::Deactivated {
                    server: "docs".into(),
                    reason: "explicit".into()
                },
            ]
        );
        assert!(!b.deactivate("docs", "explicit"));
    }

    #[test]
    fn idle_servers_expire_but_used_ones_do_not() {
        let mut b = broker("implement");
        b.activate("docs", 0);
        b.activate("a", 0);
        b.record_call("a", 500_000);
        assert_eq!(b.expire(600_000), vec!["docs".to_owned()]);
        assert_eq!(b.active(), vec!["a".to_owned()]);
    }

    #[test]
    fn a_busy_server_still_ends_at_its_maximum_lifetime() {
        let mut b = broker("implement");
        b.activate("docs", 0);
        b.record_call("docs", 3_599_000);
        assert_eq!(b.expire(3_600_000), vec!["docs".to_owned()]);
        assert!(b.take_events().iter().any(|e| matches!(
            e,
            McpEvent::Deactivated { reason, .. } if reason == "max-lifetime"
        )));
    }

    #[test]
    fn stage_end_deactivates_everything_still_active() {
        let mut b = broker("implement");
        b.activate("docs", 0);
        b.activate("a", 0);
        assert_eq!(b.end_stage(), vec!["a".to_owned(), "docs".to_owned()]);
        assert!(b.active().is_empty());
    }

    #[test]
    fn an_entry_with_neither_command_nor_url_is_rejected() {
        assert!(matches!(
            Catalog::parse("[[server]]\nname = \"x\"\n"),
            Err(CatalogError::NoLaunch(_))
        ));
    }
}
