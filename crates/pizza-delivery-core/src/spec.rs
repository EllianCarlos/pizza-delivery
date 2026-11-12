use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::handoff::sections;

pub const REQUIRED_SECTIONS: [&str; 4] = ["problem", "solution", "acceptance criteria", "seams"];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SpecError {
    #[error("spec is missing the '{0}' section")]
    MissingSection(&'static str),
    #[error("spec section '{0}' is empty")]
    EmptySection(&'static str),
    #[error("spec has no tickets")]
    NoTickets,
    #[error("ticket '{0}' has no id")]
    MissingId(String),
    #[error("ticket id '{0}' is used twice")]
    DuplicateId(String),
    #[error("ticket '{ticket}' is blocked by unknown ticket '{blocker}'")]
    UnknownBlocker { ticket: String, blocker: String },
    #[error("tickets form a cycle through '{0}'")]
    Cycle(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub id: String,
    pub blocked_by: Vec<String>,
}

pub fn validate_spec(text: &str) -> Result<(), SpecError> {
    let found = sections(text);
    for required in REQUIRED_SECTIONS {
        let (_, body) = found
            .iter()
            .find(|(title, _)| title == required)
            .ok_or(SpecError::MissingSection(required))?;
        if body.trim().is_empty() {
            return Err(SpecError::EmptySection(required));
        }
    }
    Ok(())
}

fn frontmatter_value(text: &str, key: &str) -> Option<String> {
    let rest = text.trim_start().strip_prefix("---")?;
    let end = rest.find("\n---")?;
    rest[..end]
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches('"').to_owned())
}

pub fn parse_ticket(name: &str, text: &str) -> Result<Ticket, SpecError> {
    let id = frontmatter_value(text, "id")
        .filter(|id| !id.is_empty())
        .ok_or_else(|| SpecError::MissingId(name.to_owned()))?;
    let blocked_by = frontmatter_value(text, "blocked_by")
        .map(|v| {
            v.trim_matches(['[', ']'])
                .split(',')
                .map(|b| b.trim().trim_matches('"').to_owned())
                .filter(|b| !b.is_empty())
                .collect()
        })
        .unwrap_or_default();
    Ok(Ticket { id, blocked_by })
}

pub fn validate_graph(tickets: &[Ticket]) -> Result<(), SpecError> {
    if tickets.is_empty() {
        return Err(SpecError::NoTickets);
    }
    let mut ids = BTreeSet::new();
    for ticket in tickets {
        if !ids.insert(ticket.id.as_str()) {
            return Err(SpecError::DuplicateId(ticket.id.clone()));
        }
    }
    for ticket in tickets {
        if let Some(blocker) = ticket.blocked_by.iter().find(|b| !ids.contains(b.as_str())) {
            return Err(SpecError::UnknownBlocker {
                ticket: ticket.id.clone(),
                blocker: blocker.clone(),
            });
        }
    }
    let mut waiting: BTreeMap<&str, usize> = tickets
        .iter()
        .map(|t| (t.id.as_str(), t.blocked_by.len()))
        .collect();
    let mut ready: Vec<&str> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    while let Some(done) = ready.pop() {
        waiting.remove(done);
        for ticket in tickets
            .iter()
            .filter(|t| t.blocked_by.iter().any(|b| b == done))
        {
            if let Some(n) = waiting.get_mut(ticket.id.as_str()) {
                *n -= 1;
                if *n == 0 {
                    ready.push(ticket.id.as_str());
                }
            }
        }
    }
    match waiting.keys().next() {
        Some(stuck) => Err(SpecError::Cycle((*stuck).to_owned())),
        None => Ok(()),
    }
}

pub fn validate(spec: &str, tickets: &[(String, String)]) -> Result<(), SpecError> {
    validate_spec(spec)?;
    let parsed = tickets
        .iter()
        .map(|(name, text)| parse_ticket(name, text))
        .collect::<Result<Vec<_>, _>>()?;
    validate_graph(&parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "# Spec\n\n## Problem\np\n\n## Solution\ns\n\n## Acceptance criteria\n- a\n\n## Seams\n- cli\n";

    fn ticket(id: &str, blocked_by: &[&str]) -> String {
        format!(
            "---\nid: {id}\nblocked_by: [{}]\n---\n\nbody\n",
            blocked_by.join(", ")
        )
    }

    fn named(tickets: &[(&str, &[&str])]) -> Vec<(String, String)> {
        tickets
            .iter()
            .map(|(id, blockers)| (format!("{id}.md"), ticket(id, blockers)))
            .collect()
    }

    #[test]
    fn a_spec_with_every_section_and_an_acyclic_graph_is_valid() {
        let tickets = named(&[("t1", &[]), ("t2", &["t1"]), ("t3", &["t1", "t2"])]);
        assert_eq!(validate(SPEC, &tickets), Ok(()));
    }

    #[test]
    fn a_missing_section_is_named() {
        let spec = SPEC.replace("## Seams\n- cli\n", "");
        assert_eq!(
            validate(&spec, &named(&[("t1", &[])])),
            Err(SpecError::MissingSection("seams"))
        );
    }

    #[test]
    fn an_empty_section_is_rejected() {
        let spec = SPEC.replace("## Problem\np\n", "## Problem\n\n");
        assert_eq!(
            validate_spec(&spec),
            Err(SpecError::EmptySection("problem"))
        );
    }

    #[test]
    fn no_tickets_is_rejected() {
        assert_eq!(validate(SPEC, &[]), Err(SpecError::NoTickets));
    }

    #[test]
    fn a_cycle_is_rejected() {
        let tickets = named(&[("t1", &["t3"]), ("t2", &["t1"]), ("t3", &["t2"])]);
        assert!(matches!(validate(SPEC, &tickets), Err(SpecError::Cycle(_))));
    }

    #[test]
    fn a_self_dependency_is_a_cycle() {
        assert!(matches!(
            validate(SPEC, &named(&[("t1", &["t1"])])),
            Err(SpecError::Cycle(_))
        ));
    }

    #[test]
    fn an_unknown_blocker_is_rejected() {
        assert_eq!(
            validate(SPEC, &named(&[("t1", &["ghost"])])),
            Err(SpecError::UnknownBlocker {
                ticket: "t1".into(),
                blocker: "ghost".into()
            })
        );
    }

    #[test]
    fn duplicate_and_missing_ids_are_rejected() {
        assert_eq!(
            validate(SPEC, &named(&[("t1", &[]), ("t1", &[])])),
            Err(SpecError::DuplicateId("t1".into()))
        );
        assert_eq!(
            validate(SPEC, &[("x.md".into(), "no frontmatter".into())]),
            Err(SpecError::MissingId("x.md".into()))
        );
    }
}
