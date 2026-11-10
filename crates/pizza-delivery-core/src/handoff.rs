use thiserror::Error;

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

fn sections(body: &str) -> Vec<(String, String)> {
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

    #[test]
    fn the_template_has_every_section_but_no_goal_text() {
        assert_eq!(
            validate(&template("J1", "implement")),
            Err(HandoffError::EmptyGoal)
        );
    }
}
