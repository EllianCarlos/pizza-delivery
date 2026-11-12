use thiserror::Error;

use crate::handoff::sections;
use crate::triage::Feedback;

pub const MAX_FIX_ROUNDS: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ReviewError {
    #[error("review report has no 'Findings' section")]
    MissingFindings,
    #[error("finding is not classified as [blocking] or [non-blocking]: {0}")]
    Unclassified(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub blocking: bool,
    pub text: String,
}

pub fn parse_report(text: &str) -> Result<Vec<Finding>, ReviewError> {
    let found = sections(text);
    let (_, body) = found
        .iter()
        .find(|(title, _)| title == "findings")
        .ok_or(ReviewError::MissingFindings)?;
    body.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| {
            !matches!(
                l.trim_end_matches('.').to_lowercase().as_str(),
                "none" | "- none"
            )
        })
        .map(|line| {
            let bullet = line.strip_prefix("- ").unwrap_or(line);
            let lower = bullet.to_lowercase();
            if let Some(rest) = lower.strip_prefix("[non-blocking]") {
                Ok(Finding {
                    blocking: false,
                    text: bullet[bullet.len() - rest.len()..].trim().to_owned(),
                })
            } else if let Some(rest) = lower.strip_prefix("[blocking]") {
                Ok(Finding {
                    blocking: true,
                    text: bullet[bullet.len() - rest.len()..].trim().to_owned(),
                })
            } else {
                Err(ReviewError::Unclassified(bullet.to_owned()))
            }
        })
        .collect()
}

pub fn blocking(findings: &[Finding]) -> Vec<&Finding> {
    findings.iter().filter(|f| f.blocking).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Clean,
    Fix,
    Leftover(Vec<Feedback>),
}

pub fn decide(findings: &[Finding], fix_rounds_done: u32, can_plan_fix: bool) -> Decision {
    let blocking = blocking(findings);
    if blocking.is_empty() {
        return Decision::Clean;
    }
    if fix_rounds_done < MAX_FIX_ROUNDS && can_plan_fix {
        return Decision::Fix;
    }
    Decision::Leftover(
        blocking
            .into_iter()
            .map(|f| Feedback {
                kind: "risk".to_owned(),
                severity: "high".to_owned(),
                text: format!("unresolved review finding: {}", f.text),
                confidence: None,
                evidence: None,
                alternatives: Vec::new(),
                impact: Some(format!(
                    "shipped after {fix_rounds_done} fix round(s) with this finding still open"
                )),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "# Review\n\n## Findings\n- [blocking] null deref in parser\n- [non-blocking] rename helper\n\n## Summary\nok\n";

    fn finding(blocking: bool, text: &str) -> Finding {
        Finding {
            blocking,
            text: text.into(),
        }
    }

    #[test]
    fn findings_are_classified_blocking_or_not() {
        assert_eq!(
            parse_report(REPORT),
            Ok(vec![
                finding(true, "null deref in parser"),
                finding(false, "rename helper")
            ])
        );
    }

    #[test]
    fn a_none_report_has_no_findings() {
        assert_eq!(parse_report("## Findings\nNone\n"), Ok(vec![]));
    }

    #[test]
    fn a_report_without_findings_section_or_with_unclassified_ones_is_invalid() {
        assert_eq!(
            parse_report("## Summary\nx\n"),
            Err(ReviewError::MissingFindings)
        );
        assert_eq!(
            parse_report("## Findings\n- looks odd\n"),
            Err(ReviewError::Unclassified("looks odd".into()))
        );
    }

    #[test]
    fn only_blocking_findings_plan_a_fix_and_only_under_the_cap() {
        let findings = [finding(true, "bug"), finding(false, "nit")];
        assert_eq!(decide(&[finding(false, "nit")], 0, true), Decision::Clean);
        assert_eq!(decide(&findings, 0, true), Decision::Fix);
        assert_eq!(decide(&findings, 1, true), Decision::Fix);
    }

    #[test]
    fn after_two_fix_rounds_blocking_leftovers_become_high_risk_entries() {
        let findings = [finding(true, "bug"), finding(false, "nit")];
        let entries = match decide(&findings, 2, true) {
            Decision::Leftover(entries) => entries,
            _ => Vec::new(),
        };
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries
                .first()
                .map(|e| (e.kind.as_str(), e.severity.as_str())),
            Some(("risk", "high"))
        );
        assert!(entries.first().is_some_and(|e| e.text.contains("bug")));
    }

    #[test]
    fn a_plan_that_cannot_grow_leaves_the_findings_as_risks() {
        assert!(matches!(
            decide(&[finding(true, "bug")], 0, false),
            Decision::Leftover(_)
        ));
    }
}
