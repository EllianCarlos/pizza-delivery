use crate::feedback::parse_feedback;
use crate::handoff::{body, sections};

pub const STAGE: &str = "disambiguate";

/// Wall-clock cap for the stage, in seconds, whatever the budget tier.
pub const TIMEOUT_SECS: u64 = 60;

/// Spend cap for the stage, passed to adapters that can enforce it.
pub const MAX_COST_USD: &str = "0.35";

const REPO_LINES: [&str; 3] = ["default branch", "remote", "latest commit"];

fn is_none(value: &str) -> bool {
    matches!(
        value.trim().trim_end_matches('.').to_lowercase().as_str(),
        "" | "none" | "n/a" | "unknown" | "unresolved"
    )
}

fn line_value(section: &str, key: &str) -> Option<String> {
    section.lines().find_map(|line| {
        let line = line.trim().trim_start_matches(['-', '*']).trim();
        let (name, value) = line.split_once(':')?;
        (name.trim().to_lowercase() == key).then(|| value.trim().to_owned())
    })
}

/// Deterministic check of a disambiguate handoff, on top of the generic
/// handoff validation. The agent must have resolved and verified the
/// repository (or said it could not and recorded an assumption), listed its
/// resolved references, and must not have left open questions: leftovers are
/// assumptions.
pub fn validate(handoff: &str) -> Result<(), String> {
    let text = body(handoff).unwrap_or(handoff);
    let found = sections(text);
    let section = |title: &str| {
        found
            .iter()
            .find(|(name, _)| name == title)
            .map_or("", |(_, body)| body.as_str())
    };
    let done = section("what was done");
    let repository = line_value(done, "repository")
        .ok_or_else(|| "missing a 'Repository:' line under What was done".to_owned())?;
    line_value(done, "resolved references")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            "missing a 'Resolved references:' line under What was done (write 'none' when there are none)"
                .to_owned()
        })?;
    if repository.is_empty() {
        return Err("the 'Repository:' line is empty".to_owned());
    }
    if repository.to_lowercase().starts_with("unresolved") {
        let recorded = parse_feedback(text)
            .iter()
            .any(|f| f.kind == "assumption" && f.text.to_lowercase().contains("repositor"));
        if !recorded {
            return Err(
                "the repository is unresolved but no Assumptions bullet records which repository is assumed"
                    .to_owned(),
            );
        }
    } else {
        for key in REPO_LINES {
            let value = line_value(done, key).unwrap_or_default();
            if is_none(&value) {
                return Err(format!(
                    "the repository was not verified: the '{key}' line under What was done is missing or empty"
                ));
            }
        }
    }
    let has_question = section("open questions")
        .lines()
        .filter_map(|l| l.trim().strip_prefix("- "))
        .map(str::trim)
        .any(|b| !b.is_empty() && !is_none(b) && b.to_lowercase() != "nothing");
    if has_question {
        return Err(
            "Open questions must be empty: record every leftover as an assumption instead"
                .to_owned(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff(done: &str, assumptions: &str, questions: &str) -> String {
        format!(
            "---\njob: J\nstage: disambiguate\n---\n\n## Goal\nadd a flag\n\n## What was done\n{done}\n\n## Decisions made\nnone\n\n## Assumptions\n{assumptions}\n\n## Open questions\n{questions}\n\n## Files touched\nnone\n\n## Instructions for the next stage\ngo\n"
        )
    }

    const VERIFIED: &str = "- Repository: /home/me/app\n- Default branch: main\n- Remote: git@github.com:me/app.git\n- Latest commit: abc1234\n- Resolved references: none";

    #[test]
    fn a_verified_repository_with_no_questions_passes() {
        assert_eq!(validate(&handoff(VERIFIED, "none", "none")), Ok(()));
    }

    #[test]
    fn a_missing_repository_line_fails() {
        let err = validate(&handoff("- Resolved references: none", "none", "none"));
        assert!(err.is_err_and(|e| e.contains("Repository")));
    }

    #[test]
    fn a_repository_without_a_default_branch_remote_or_commit_fails() {
        let done = "- Repository: /home/me/app\n- Default branch: main\n- Remote: none\n- Latest commit: abc\n- Resolved references: none";
        let err = validate(&handoff(done, "none", "none"));
        assert!(err.is_err_and(|e| e.contains("remote")));
    }

    #[test]
    fn an_unresolved_repository_needs_an_assumption() {
        let done = "- Repository: unresolved\n- Resolved references: none";
        assert!(validate(&handoff(done, "none", "none")).is_err());
        let assumption =
            "- Assumed the repository is the current one [confidence: 40; alternatives: other; impact: high; evidence: cwd]";
        assert_eq!(validate(&handoff(done, assumption, "none")), Ok(()));
    }

    #[test]
    fn open_questions_are_rejected_but_none_is_fine() {
        let err = validate(&handoff(VERIFIED, "none", "- Which db?"));
        assert!(err.is_err_and(|e| e.contains("Open questions")));
        assert_eq!(validate(&handoff(VERIFIED, "none", "- none")), Ok(()));
    }

    #[test]
    fn the_stage_is_capped_at_a_minute_and_thirty_five_cents() {
        assert_eq!(TIMEOUT_SECS, 60);
        assert_eq!(MAX_COST_USD, "0.35");
    }
}
