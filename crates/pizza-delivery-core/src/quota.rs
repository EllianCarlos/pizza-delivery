use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuotaStatus {
    Allowed,
    Exhausted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AgentQuota {
    pub utilization_permille: Option<u32>,
    pub resets_at_ms: Option<i64>,
    pub exhausted_until_ms: Option<i64>,
    pub reset_known: bool,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuotaSnapshot {
    agents: BTreeMap<String, AgentQuota>,
}

impl QuotaSnapshot {
    pub fn get(&self, agent: &str) -> Option<&AgentQuota> {
        self.agents.get(agent)
    }

    pub fn agents(&self) -> impl Iterator<Item = (&String, &AgentQuota)> {
        self.agents.iter()
    }

    pub fn with_signal(mut self, event: &Event) -> Self {
        if let EventPayload::QuotaSignal {
            agent,
            status,
            utilization_permille,
            resets_at_ms,
            reset_known,
            ..
        } = &event.payload
        {
            let exhausted_until_ms = match status {
                QuotaStatus::Allowed => None,
                QuotaStatus::Exhausted => *resets_at_ms,
            };
            self.agents.insert(
                agent.clone(),
                AgentQuota {
                    utilization_permille: *utilization_permille,
                    resets_at_ms: *resets_at_ms,
                    exhausted_until_ms,
                    reset_known: *reset_known,
                    observed_at_ms: event.ts_ms,
                },
            );
        }
        self
    }
}

pub fn fold_quota(events: &[Event]) -> QuotaSnapshot {
    events
        .iter()
        .fold(QuotaSnapshot::default(), QuotaSnapshot::with_signal)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct QuotaView {
    pub agent: String,
    pub state: String,
    pub utilization_permille: Option<u32>,
    pub resets_at_ms: Option<i64>,
    pub reset_known: bool,
}

pub fn quota_views(snapshot: &QuotaSnapshot, now_ms: i64) -> Vec<QuotaView> {
    snapshot
        .agents()
        .map(|(agent, q)| {
            let exhausted = q.exhausted_until_ms.is_some_and(|until| until > now_ms);
            QuotaView {
                agent: agent.clone(),
                state: if exhausted { "exhausted" } else { "available" }.to_owned(),
                utilization_permille: q.utilization_permille,
                resets_at_ms: q.resets_at_ms,
                reset_known: q.reset_known,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;
    use crate::id::JobId;

    fn signal(
        seq: u64,
        agent: &str,
        status: QuotaStatus,
        utilization: Option<u32>,
        resets: Option<i64>,
    ) -> Event {
        Event {
            id: format!("e{seq}"),
            job_id: JobId::new("J"),
            stage_id: None,
            seq,
            ts_ms: seq as i64 * 10,
            actor: Actor::Daemon,
            payload: EventPayload::QuotaSignal {
                agent: agent.to_owned(),
                model: None,
                status,
                utilization_permille: utilization,
                resets_at_ms: resets,
                reset_known: resets.is_some(),
            },
        }
    }

    #[test]
    fn the_latest_signal_per_agent_wins() {
        let snap = fold_quota(&[
            signal(1, "claude", QuotaStatus::Exhausted, None, Some(900)),
            signal(2, "opencode", QuotaStatus::Allowed, Some(100), Some(500)),
            signal(3, "claude", QuotaStatus::Allowed, Some(300), Some(800)),
        ]);
        assert_eq!(snap.get("claude").map(|q| q.exhausted_until_ms), Some(None));
        assert_eq!(
            snap.get("claude").map(|q| q.utilization_permille),
            Some(Some(300))
        );
        assert_eq!(
            snap.get("opencode").map(|q| q.utilization_permille),
            Some(Some(100))
        );
    }

    #[test]
    fn an_exhausted_agent_is_listed_exhausted_only_until_its_reset() {
        let snap = fold_quota(&[signal(1, "claude", QuotaStatus::Exhausted, None, Some(900))]);
        let before = quota_views(&snap, 800);
        let after = quota_views(&snap, 900);
        assert_eq!(before.first().map(|v| v.state.as_str()), Some("exhausted"));
        assert_eq!(after.first().map(|v| v.state.as_str()), Some("available"));
    }
}
