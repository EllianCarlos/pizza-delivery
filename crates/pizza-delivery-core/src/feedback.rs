use crate::handoff::sections;
use crate::triage::Feedback;

const SOURCES: [(&str, &str, &str); 3] = [
    ("assumptions", "assumption", "low"),
    ("open questions", "question", "medium"),
    ("risks", "risk", "medium"),
];

struct Attributes {
    confidence: Option<u8>,
    severity: Option<String>,
    alternatives: Vec<String>,
    impact: Option<String>,
}

fn split_attributes(bullet: &str) -> (&str, Attributes) {
    let empty = Attributes {
        confidence: None,
        severity: None,
        alternatives: Vec::new(),
        impact: None,
    };
    let Some(open) = bullet.rfind('[') else {
        return (bullet, empty);
    };
    let Some(inner) = bullet[open + 1..].strip_suffix(']') else {
        return (bullet, empty);
    };
    let attributes = inner.split(';').fold(empty, |mut acc, part| {
        if let Some((key, value)) = part.split_once(':') {
            let value = value.trim();
            match key.trim() {
                "confidence" => acc.confidence = value.parse::<u8>().ok().map(|c| c.min(100)),
                "severity" => acc.severity = Some(value.to_lowercase()),
                "alternatives" => {
                    acc.alternatives = value
                        .split(',')
                        .map(|a| a.trim().to_owned())
                        .filter(|a| !a.is_empty())
                        .collect();
                }
                "impact" => acc.impact = Some(value.to_owned()).filter(|i| !i.is_empty()),
                _ => {}
            }
        }
        acc
    });
    (bullet[..open].trim_end(), attributes)
}

fn is_none(text: &str) -> bool {
    matches!(
        text.trim_end_matches('.').to_lowercase().as_str(),
        "none" | "n/a" | "nothing"
    )
}

pub fn parse_feedback(handoff: &str) -> Vec<Feedback> {
    let found = sections(handoff);
    SOURCES
        .iter()
        .flat_map(|(title, kind, default_severity)| {
            found
                .iter()
                .filter(move |(name, _)| name == title)
                .flat_map(|(_, body)| body.lines())
                .filter_map(|line| line.trim().strip_prefix("- "))
                .map(str::trim)
                .filter(|bullet| !bullet.is_empty() && !is_none(bullet))
                .map(move |bullet| {
                    let (text, attributes) = split_attributes(bullet);
                    Feedback {
                        kind: (*kind).to_owned(),
                        severity: attributes
                            .severity
                            .unwrap_or_else(|| (*default_severity).to_owned()),
                        text: text.to_owned(),
                        confidence: attributes.confidence,
                        alternatives: attributes.alternatives,
                        impact: attributes.impact,
                    }
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDOFF: &str = "---\njob: J\nstage: grill\n---\n\n## Goal\ng\n\n## Assumptions\n- The API stays v1 [confidence: 70; alternatives: bump to v2, keep both; impact: clients break on v2]\n- Tests run offline\n\n## Open questions\n- Which database? [confidence: 20; severity: high]\n\n## Risks\n- Migration may lock the table [severity: high; impact: downtime]\n";

    fn entry(kind: &str) -> Option<Feedback> {
        parse_feedback(HANDOFF).into_iter().find(|f| f.kind == kind)
    }

    #[test]
    fn an_assumption_carries_confidence_alternatives_and_impact() {
        assert_eq!(
            entry("assumption"),
            Some(Feedback {
                kind: "assumption".into(),
                severity: "low".into(),
                text: "The API stays v1".into(),
                confidence: Some(70),
                alternatives: vec!["bump to v2".into(), "keep both".into()],
                impact: Some("clients break on v2".into()),
            })
        );
    }

    #[test]
    fn a_bullet_without_attributes_has_none_of_them() {
        let all = parse_feedback(HANDOFF);
        assert_eq!(
            all.get(1),
            Some(&Feedback {
                kind: "assumption".into(),
                severity: "low".into(),
                text: "Tests run offline".into(),
                confidence: None,
                alternatives: vec![],
                impact: None,
            })
        );
    }

    #[test]
    fn questions_and_risks_take_their_own_severity() {
        let question = entry("question");
        let risk = entry("risk");
        assert_eq!(
            question.map(|q| (q.severity, q.confidence)),
            Some(("high".to_owned(), Some(20)))
        );
        assert_eq!(
            risk.map(|r| (r.severity, r.impact)),
            Some(("high".to_owned(), Some("downtime".to_owned())))
        );
    }

    #[test]
    fn none_placeholders_and_missing_sections_yield_nothing() {
        let text =
            "---\njob: J\nstage: s\n---\n\n## Assumptions\n- none\n\n## Open questions\nNone.\n";
        assert_eq!(parse_feedback(text), vec![]);
    }
}
