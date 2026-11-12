use thiserror::Error;

use crate::gate::Impact;

pub const REQUIRED_SECTIONS: [&str; 7] = [
    "goal",
    "what was done",
    "decisions made",
    "assumptions",
    "open questions",
    "files touched",
    "instructions for the next stage",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub job: String,
    pub stage: String,
    pub goal: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HandoffError {
    #[error("handoff has no frontmatter")]
    MissingFrontmatter,
    #[error("handoff frontmatter is missing '{0}'")]
    MissingField(&'static str),
    #[error("handoff is missing the '{0}' section")]
    MissingSection(&'static str),
    #[error("handoff goal section is empty")]
    EmptyGoal,
}

fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let rest = text.trim_start().strip_prefix("---")?;
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    Some((&rest[..end], body))
}

fn field(front: &str, key: &'static str) -> Result<String, HandoffError> {
    front
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim() == key)
        .map(|(_, v)| v.trim().trim_matches('"').to_owned())
        .filter(|v| !v.is_empty())
        .ok_or(HandoffError::MissingField(key))
}

pub(crate) fn sections(body: &str) -> Vec<(String, String)> {
    body.lines().fold(Vec::new(), |mut acc, line| {
        match line.strip_prefix("## ") {
            Some(title) => acc.push((title.trim().to_lowercase(), String::new())),
            None => {
                if let Some((_, text)) = acc.last_mut() {
                    text.push_str(line);
                    text.push('\n');
                }
            }
        }
        acc
    })
}

pub(crate) fn body(text: &str) -> Option<&str> {
    split_frontmatter(text).map(|(_, body)| body)
}

pub fn validate(text: &str) -> Result<Handoff, HandoffError> {
    let (front, body) = split_frontmatter(text).ok_or(HandoffError::MissingFrontmatter)?;
    let job = field(front, "job")?;
    let stage = field(front, "stage")?;
    let found = sections(body);
    for required in REQUIRED_SECTIONS {
        if !found.iter().any(|(title, _)| title == required) {
            return Err(HandoffError::MissingSection(required));
        }
    }
    let goal = found
        .iter()
        .find(|(title, _)| title == "goal")
        .map(|(_, text)| text.trim().to_owned())
        .filter(|g| !g.is_empty())
        .ok_or(HandoffError::EmptyGoal)?;
    Ok(Handoff { job, stage, goal })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Assumption,
    OpenQuestion,
    Risk,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assumption => "assumption",
            Self::OpenQuestion => "open-question",
            Self::Risk => "risk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffItem {
    pub kind: ItemKind,
    pub text: String,
    pub confidence: Option<u8>,
    pub alternatives: Vec<String>,
    pub impact: Option<Impact>,
    pub evidence: Option<String>,
}

#[derive(Default)]
struct Meta {
    confidence: Option<u8>,
    alternatives: Vec<String>,
    impact: Option<Impact>,
    evidence: Option<String>,
}

fn parse_meta(inner: &str) -> Option<Meta> {
    let mut meta = Meta::default();
    let mut known = false;
    for part in inner.split(';') {
        let Some((key, value)) = part.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_lowercase().as_str() {
            "confidence" => {
                known = true;
                meta.confidence = value
                    .trim_end_matches('%')
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|c| *c <= 100);
            }
            "alternatives" => {
                known = true;
                meta.alternatives = value
                    .split(',')
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            "impact" => {
                known = true;
                meta.impact = Impact::parse(value);
            }
            "evidence" => {
                known = true;
                meta.evidence = Some(value.to_owned()).filter(|v| !v.is_empty());
            }
            _ => {}
        }
    }
    known.then_some(meta)
}

fn item_from(kind: ItemKind, bullet: &str) -> Option<HandoffItem> {
    let bullet = bullet.trim();
    let (text, meta) = match bullet
        .strip_suffix(']')
        .and_then(|b| b.rfind('[').map(|i| (b, i)))
    {
        Some((b, i)) => match parse_meta(&b[i + 1..]) {
            Some(meta) => (b[..i].trim(), meta),
            None => (bullet, Meta::default()),
        },
        None => (bullet, Meta::default()),
    };
    let lowered = text.trim_end_matches('.').to_lowercase();
    if matches!(lowered.as_str(), "" | "none" | "n/a" | "nothing") {
        return None;
    }
    Some(HandoffItem {
        kind,
        text: text.to_owned(),
        confidence: meta.confidence,
        alternatives: meta.alternatives,
        impact: meta.impact,
        evidence: meta.evidence,
    })
}

/// Bullets of the Assumptions, Open questions and Risks sections. Missing or
/// malformed metadata yields unknown fields; a bullet is never dropped for it.
pub fn items(text: &str) -> Vec<HandoffItem> {
    let Some((_, body)) = split_frontmatter(text) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (title, content) in sections(body) {
        let kind = match title.as_str() {
            "assumptions" => ItemKind::Assumption,
            "open questions" => ItemKind::OpenQuestion,
            "risks" => ItemKind::Risk,
            _ => continue,
        };
        let mut bullets: Vec<String> = Vec::new();
        for line in content.lines() {
            let trimmed = line.trim();
            let numbered = trimmed
                .split_once(". ")
                .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                .map(|(_, rest)| rest);
            if let Some(rest) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or(numbered)
            {
                bullets.push(rest.trim().to_owned());
            } else if let Some(last) = bullets
                .last_mut()
                .filter(|_| !trimmed.is_empty() && line.starts_with([' ', '\t']))
            {
                last.push(' ');
                last.push_str(trimmed);
            }
        }
        out.extend(bullets.iter().filter_map(|b| item_from(kind, b)));
    }
    out
}

pub fn template(job: &str, stage: &str) -> String {
    let sections: String = REQUIRED_SECTIONS
        .iter()
        .map(|s| {
            let mut title = s.to_string();
            if let Some(first) = title.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            format!("## {title}\n\n")
        })
        .collect();
    format!("---\njob: {job}\nstage: {stage}\n---\n\n{sections}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "---\njob: J1\nstage: implement\n---\n\n## Goal\nfix it\n\n## What was done\nx\n\n## Decisions made\nx\n\n## Assumptions\nnone\n\n## Open questions\nnone\n\n## Files touched\na.rs\n\n## Instructions for the next stage\nreview\n";

    #[test]
    fn a_complete_handoff_validates() {
        assert_eq!(
            validate(GOOD),
            Ok(Handoff {
                job: "J1".into(),
                stage: "implement".into(),
                goal: "fix it".into()
            })
        );
    }

    #[test]
    fn missing_frontmatter_is_rejected() {
        assert_eq!(
            validate("## Goal\nfix it\n"),
            Err(HandoffError::MissingFrontmatter)
        );
    }

    #[test]
    fn missing_frontmatter_field_is_rejected() {
        let text = GOOD.replace("stage: implement\n", "");
        assert_eq!(validate(&text), Err(HandoffError::MissingField("stage")));
    }

    #[test]
    fn missing_section_is_rejected_by_name() {
        let text = GOOD.replace("## Files touched\na.rs\n\n", "");
        assert_eq!(
            validate(&text),
            Err(HandoffError::MissingSection("files touched"))
        );
    }

    #[test]
    fn empty_goal_is_rejected() {
        let text = GOOD.replace("## Goal\nfix it\n", "## Goal\n\n");
        assert_eq!(validate(&text), Err(HandoffError::EmptyGoal));
    }

    fn doc(assumptions: &str, questions: &str, risks: &str) -> String {
        GOOD.replace(
            "## Assumptions\nnone\n",
            &format!("## Assumptions\n{assumptions}\n"),
        )
        .replace(
            "## Open questions\nnone\n",
            &format!("## Open questions\n{questions}\n"),
        ) + &format!("\n## Risks\n{risks}\n")
    }

    #[test]
    fn well_formed_bullets_are_parsed_in_all_three_sections() {
        let text = doc(
            "- use sqlite [confidence: 90; alternatives: postgres, files; impact: low; evidence: Cargo.toml]",
            "* which repo? [confidence: 30; alternatives: a; impact: high]",
            "- may leak [confidence: 60; impact: irreversible]",
        );
        let got = items(&text);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].kind, ItemKind::Assumption);
        assert_eq!(got[0].text, "use sqlite");
        assert_eq!(got[0].confidence, Some(90));
        assert_eq!(got[0].alternatives, vec!["postgres", "files"]);
        assert_eq!(got[0].impact, Some(Impact::Low));
        assert_eq!(got[0].evidence.as_deref(), Some("Cargo.toml"));
        assert_eq!(got[1].kind, ItemKind::OpenQuestion);
        assert_eq!(got[1].impact, Some(Impact::High));
        assert_eq!(got[2].kind, ItemKind::Risk);
        assert_eq!(got[2].impact, Some(Impact::Irreversible));
    }

    #[test]
    fn whitespace_is_tolerated() {
        let got = items(&doc(
            "   -   spaced   [ confidence :  75 ;impact:Medium ]  ",
            "none",
            "none",
        ));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "spaced");
        assert_eq!(got[0].confidence, Some(75));
        assert_eq!(got[0].impact, Some(Impact::Medium));
    }

    #[test]
    fn malformed_or_missing_metadata_keeps_the_bullet_with_unknown_fields() {
        let got = items(&doc(
            "- plain\n- bad [confidence: lots; impact: huge]\n- over [confidence: 250; impact: low]",
            "none",
            "none",
        ));
        assert_eq!(got.len(), 3);
        assert!(got.iter().all(|i| i.confidence.is_none()));
        assert_eq!(got[0].text, "plain");
        assert_eq!(got[1].impact, None);
        assert_eq!(got[2].impact, Some(Impact::Low));
    }

    #[test]
    fn empty_sections_and_none_yield_nothing() {
        assert!(items(&doc("none", "None.", "")).is_empty());
        assert!(items(&doc("- none", "prose without a bullet", "")).is_empty());
        assert!(items("no frontmatter").is_empty());
    }

    #[test]
    fn validate_is_unchanged_by_item_metadata() {
        assert!(validate(&doc("- x [confidence: 1]", "none", "none")).is_ok());
    }

    #[test]
    fn the_template_has_every_section_but_no_goal_text() {
        assert_eq!(
            validate(&template("J1", "implement")),
            Err(HandoffError::EmptyGoal)
        );
    }
}
