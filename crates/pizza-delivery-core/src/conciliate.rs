//! Pure planning for the conciliate step: order open pizza PRs into a stack
//! and decide, per PR, whether it is up to date, can take `main` mechanically,
//! or has a conflict that must be reported instead of guessed at.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPr {
    pub number: u64,
    pub branch: String,
    pub head: String,
    pub url: String,
}

/// Facts gathered from git; everything the planner needs and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanInput {
    /// The default branch as a ref, e.g. `origin/main`.
    pub base: String,
    pub prs: Vec<OpenPr>,
    /// PRs whose head does not contain `base`.
    pub behind: BTreeSet<u64>,
    /// Conflicted paths when merging `base` into a PR; absent or empty is clean.
    pub base_conflicts: BTreeMap<u64, Vec<String>>,
    /// `(outer, inner)`: the outer PR's head contains the inner PR's head.
    pub contains: BTreeSet<(u64, u64)>,
    /// Conflicted paths between two PRs, keyed with the smaller number first.
    pub pair_conflicts: BTreeMap<(u64, u64), Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConflictWith {
    Base,
    Pr(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum Action {
    UpToDate {
        pr: u64,
    },
    MergeBase {
        pr: u64,
    },
    Conflict {
        pr: u64,
        with: ConflictWith,
        files: Vec<String>,
    },
}

impl Action {
    pub fn pr(&self) -> u64 {
        match self {
            Action::UpToDate { pr } | Action::MergeBase { pr } | Action::Conflict { pr, .. } => *pr,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub base: String,
    /// PR numbers in merge order.
    pub stack: Vec<u64>,
    pub prs: Vec<OpenPr>,
    pub actions: Vec<Action>,
}

impl Plan {
    pub fn pr(&self, number: u64) -> Option<&OpenPr> {
        self.prs.iter().find(|p| p.number == number)
    }
}

/// Merge order: a PR containing another's head goes after it; otherwise by number.
fn stack_order(input: &PlanInput) -> Vec<u64> {
    let mut remaining: BTreeSet<u64> = input.prs.iter().map(|p| p.number).collect();
    let mut order = Vec::new();
    while !remaining.is_empty() {
        // A PR is ready when no still-remaining PR is contained in it. Mutual
        // containment (identical heads) is not a dependency in either direction.
        let ready = remaining.iter().copied().find(|&outer| {
            !remaining.iter().any(|&inner| {
                inner != outer
                    && input.contains.contains(&(outer, inner))
                    && !input.contains.contains(&(inner, outer))
            })
        });
        // A cycle cannot happen with real ancestry; fall back to number order.
        let next = ready.or_else(|| remaining.iter().next().copied());
        match next {
            Some(n) => {
                remaining.remove(&n);
                order.push(n);
            }
            None => break,
        }
    }
    order
}

pub fn plan(input: &PlanInput) -> Plan {
    let stack = stack_order(input);
    let position = |n: u64| stack.iter().position(|&s| s == n).unwrap_or(usize::MAX);
    let mut actions = Vec::new();
    for &pr in &stack {
        let base_files = input.base_conflicts.get(&pr).filter(|f| !f.is_empty());
        if let Some(files) = base_files {
            actions.push(Action::Conflict {
                pr,
                with: ConflictWith::Base,
                files: files.clone(),
            });
        } else if input.behind.contains(&pr) {
            actions.push(Action::MergeBase { pr });
        } else {
            actions.push(Action::UpToDate { pr });
        }
        for (&(a, b), files) in &input.pair_conflicts {
            if files.is_empty()
                || input.contains.contains(&(a, b))
                || input.contains.contains(&(b, a))
            {
                continue;
            }
            let (earlier, later) = if position(a) <= position(b) {
                (a, b)
            } else {
                (b, a)
            };
            if later == pr {
                actions.push(Action::Conflict {
                    pr,
                    with: ConflictWith::Pr(earlier),
                    files: files.clone(),
                });
            }
        }
    }
    Plan {
        base: input.base.clone(),
        stack,
        prs: input.prs.clone(),
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(n: u64) -> OpenPr {
        OpenPr {
            number: n,
            branch: format!("pizza/job{n}"),
            head: format!("sha{n}"),
            url: format!("https://example.test/pull/{n}"),
        }
    }

    fn input(numbers: &[u64]) -> PlanInput {
        PlanInput {
            base: "origin/main".into(),
            prs: numbers.iter().map(|&n| pr(n)).collect(),
            ..PlanInput::default()
        }
    }

    #[test]
    fn independent_prs_are_ordered_by_number() {
        let p = plan(&input(&[25, 23, 24]));
        assert_eq!(p.stack, vec![23, 24, 25]);
    }

    #[test]
    fn a_pr_containing_another_goes_after_it_whatever_the_numbers() {
        let mut i = input(&[10, 11, 12]);
        // 10 contains 12, 12 contains 11: merge 11, 12, 10.
        i.contains.insert((10, 12));
        i.contains.insert((10, 11));
        i.contains.insert((12, 11));
        assert_eq!(plan(&i).stack, vec![11, 12, 10]);
    }

    #[test]
    fn identical_heads_are_not_a_dependency() {
        let mut i = input(&[2, 1]);
        i.contains.insert((1, 2));
        i.contains.insert((2, 1));
        assert_eq!(plan(&i).stack, vec![1, 2]);
    }

    #[test]
    fn behind_and_clean_merges_base_and_up_to_date_does_nothing() {
        let mut i = input(&[1, 2]);
        i.behind.insert(1);
        assert_eq!(
            plan(&i).actions,
            vec![Action::MergeBase { pr: 1 }, Action::UpToDate { pr: 2 }]
        );
    }

    #[test]
    fn a_base_conflict_is_reported_and_never_merged() {
        let mut i = input(&[1]);
        i.behind.insert(1);
        i.base_conflicts.insert(1, vec!["src/a.rs".into()]);
        assert_eq!(
            plan(&i).actions,
            vec![Action::Conflict {
                pr: 1,
                with: ConflictWith::Base,
                files: vec!["src/a.rs".into()],
            }]
        );
    }

    #[test]
    fn a_pair_conflict_is_reported_on_the_later_pr_only() {
        let mut i = input(&[7, 8]);
        i.pair_conflicts.insert((7, 8), vec!["x".into()]);
        let actions = plan(&i).actions;
        assert_eq!(
            actions,
            vec![
                Action::UpToDate { pr: 7 },
                Action::UpToDate { pr: 8 },
                Action::Conflict {
                    pr: 8,
                    with: ConflictWith::Pr(7),
                    files: vec!["x".into()],
                },
            ]
        );
    }

    #[test]
    fn stacked_prs_are_not_reported_as_conflicting_with_each_other() {
        let mut i = input(&[1, 2]);
        i.contains.insert((2, 1));
        i.pair_conflicts.insert((1, 2), vec!["x".into()]);
        assert!(plan(&i)
            .actions
            .iter()
            .all(|a| !matches!(a, Action::Conflict { .. })));
    }

    #[test]
    fn plan_is_deterministic_and_empty_input_is_an_empty_plan() {
        let i = input(&[]);
        assert_eq!(plan(&i), plan(&i));
        assert!(plan(&i).actions.is_empty());
    }
}
