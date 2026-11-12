//! Confidence gate: decides which handoff items reach the human inbox.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Impact {
    Low,
    Medium,
    High,
    Irreversible,
}

impl Impact {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "irreversible" => Some(Self::Irreversible),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Irreversible => "irreversible",
        }
    }

    fn is_severe(self) -> bool {
        matches!(self, Self::High | Self::Irreversible)
    }
}

pub const DEFAULT_INBOX_BELOW: u8 = 50;
pub const DEFAULT_AUTO_AT: u8 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateThresholds {
    pub inbox_below: u8,
    pub auto_at: u8,
}

impl Default for GateThresholds {
    fn default() -> Self {
        Self {
            inbox_below: DEFAULT_INBOX_BELOW,
            auto_at: DEFAULT_AUTO_AT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GateItem {
    pub confidence: Option<u8>,
    pub impact: Option<Impact>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GateDecision {
    Inbox,
    HandoffOnly,
    AutoResolved,
}

impl GateDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::HandoffOnly => "handoff-only",
            Self::AutoResolved => "auto-resolved",
        }
    }
}

/// Unknown confidence or impact fails safe to the inbox.
pub fn gate(item: GateItem, thresholds: GateThresholds) -> GateDecision {
    let (Some(confidence), Some(impact)) = (item.confidence, item.impact) else {
        return GateDecision::Inbox;
    };
    if confidence >= thresholds.auto_at {
        GateDecision::AutoResolved
    } else if confidence < thresholds.inbox_below && impact.is_severe() {
        GateDecision::Inbox
    } else {
        GateDecision::HandoffOnly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(confidence: Option<u8>, impact: Option<Impact>) -> GateDecision {
        gate(GateItem { confidence, impact }, GateThresholds::default())
    }

    #[test]
    fn the_default_table_holds_for_every_cell() {
        use GateDecision::*;
        use Impact::*;
        for (confidence, impact, want) in [
            (10, Low, HandoffOnly),
            (10, Medium, HandoffOnly),
            (10, High, Inbox),
            (10, Irreversible, Inbox),
            (60, Low, HandoffOnly),
            (60, Medium, HandoffOnly),
            (60, High, HandoffOnly),
            (60, Irreversible, HandoffOnly),
            (90, Low, AutoResolved),
            (90, Medium, AutoResolved),
            (90, High, AutoResolved),
            (90, Irreversible, AutoResolved),
        ] {
            assert_eq!(
                decide(Some(confidence), Some(impact)),
                want,
                "{confidence} {impact:?}"
            );
        }
    }

    #[test]
    fn the_boundaries_are_exact() {
        use GateDecision::*;
        assert_eq!(decide(Some(49), Some(Impact::High)), Inbox);
        assert_eq!(decide(Some(50), Some(Impact::High)), HandoffOnly);
        assert_eq!(decide(Some(79), Some(Impact::Irreversible)), HandoffOnly);
        assert_eq!(decide(Some(80), Some(Impact::Irreversible)), AutoResolved);
    }

    #[test]
    fn custom_thresholds_move_the_boundaries() {
        let t = GateThresholds {
            inbox_below: 30,
            auto_at: 70,
        };
        let g = |c| {
            gate(
                GateItem {
                    confidence: Some(c),
                    impact: Some(Impact::High),
                },
                t,
            )
        };
        assert_eq!(g(29), GateDecision::Inbox);
        assert_eq!(g(30), GateDecision::HandoffOnly);
        assert_eq!(g(70), GateDecision::AutoResolved);
    }

    #[test]
    fn unknown_confidence_or_impact_goes_to_the_inbox() {
        assert_eq!(decide(None, Some(Impact::Low)), GateDecision::Inbox);
        assert_eq!(decide(Some(95), None), GateDecision::Inbox);
        assert_eq!(decide(None, None), GateDecision::Inbox);
    }

    #[test]
    fn impact_parses_case_insensitively() {
        assert_eq!(Impact::parse(" High "), Some(Impact::High));
        assert_eq!(Impact::parse("huge"), None);
    }
}
