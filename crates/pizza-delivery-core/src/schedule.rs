use serde::{Deserialize, Serialize};

use crate::id::JobId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::Low => "low",
            Priority::Normal => "normal",
            Priority::High => "high",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "low" => Some(Priority::Low),
            "normal" => Some(Priority::Normal),
            "high" => Some(Priority::High),
            _ => None,
        }
    }

    pub fn rank(self) -> i64 {
        match self {
            Priority::Low => 0,
            Priority::Normal => 1,
            Priority::High => 2,
        }
    }

    pub fn from_rank(rank: i64) -> Self {
        match rank {
            0 => Priority::Low,
            2.. => Priority::High,
            _ => Priority::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub global: usize,
    pub per_agent: usize,
    pub per_repo: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            global: 2,
            per_agent: 2,
            per_repo: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub job_id: JobId,
    pub priority: Priority,
    pub submitted_at_ms: i64,
    pub repo: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub repo: String,
    pub agent: String,
}

pub fn pick(candidates: &[Candidate], running: &[Slot], agent: &str, caps: Caps) -> Option<usize> {
    if running.len() >= caps.global {
        return None;
    }
    if running.iter().filter(|s| s.agent == agent).count() >= caps.per_agent {
        return None;
    }
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    order.sort_by(|&a, &b| {
        let (x, y) = (&candidates[a], &candidates[b]);
        y.priority
            .cmp(&x.priority)
            .then(x.submitted_at_ms.cmp(&y.submitted_at_ms))
            .then(x.job_id.as_str().cmp(y.job_id.as_str()))
    });
    order.into_iter().find(|&i| {
        running
            .iter()
            .filter(|s| s.repo == candidates[i].repo)
            .count()
            < caps.per_repo
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: &str, priority: Priority, at: i64, repo: &str) -> Candidate {
        Candidate {
            job_id: JobId::new(id),
            priority,
            submitted_at_ms: at,
            repo: repo.to_owned(),
        }
    }

    fn slot(repo: &str, agent: &str) -> Slot {
        Slot {
            repo: repo.to_owned(),
            agent: agent.to_owned(),
        }
    }

    fn wide() -> Caps {
        Caps {
            global: 10,
            per_agent: 10,
            per_repo: 10,
        }
    }

    #[test]
    fn higher_priority_goes_first_then_earlier_submission() {
        let c = vec![
            cand("a", Priority::Normal, 1, "r"),
            cand("b", Priority::High, 5, "r"),
            cand("c", Priority::High, 3, "r"),
            cand("d", Priority::Low, 0, "r"),
        ];
        assert_eq!(pick(&c, &[], "claude", wide()), Some(2));
    }

    #[test]
    fn nothing_is_picked_when_the_global_cap_is_reached() {
        let c = vec![cand("a", Priority::Normal, 1, "r")];
        let caps = Caps {
            global: 1,
            ..wide()
        };
        assert_eq!(pick(&c, &[slot("x", "other")], "claude", caps), None);
    }

    #[test]
    fn nothing_is_picked_when_the_agent_cap_is_reached() {
        let c = vec![cand("a", Priority::Normal, 1, "r")];
        let caps = Caps {
            per_agent: 1,
            ..wide()
        };
        assert_eq!(pick(&c, &[slot("x", "claude")], "claude", caps), None);
        assert_eq!(pick(&c, &[slot("x", "codex")], "claude", caps), Some(0));
    }

    #[test]
    fn a_full_repo_is_skipped_in_favour_of_the_next_candidate() {
        let c = vec![
            cand("a", Priority::High, 1, "busy"),
            cand("b", Priority::Low, 2, "free"),
        ];
        let caps = Caps {
            per_repo: 1,
            ..wide()
        };
        assert_eq!(pick(&c, &[slot("busy", "claude")], "claude", caps), Some(1));
    }

    #[test]
    fn priority_ranks_round_trip() {
        for p in [Priority::Low, Priority::Normal, Priority::High] {
            assert_eq!(Priority::from_rank(p.rank()), p);
            assert_eq!(Priority::parse(p.as_str()), Some(p));
        }
    }
}
