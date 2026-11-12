use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::grill::DEFAULT_ROUND_CAP;
use crate::handoff;

const PREAMBLE: &str = "You are running unattended. Never ask the human anything. Wherever a skill or instruction says to ask, interview or check with the user, take the recommended answer and note it as an assumption in the handoff. Record every assumption you make, every question you could not answer and every risk you notice as a bullet in the handoff sections Assumptions, Open questions and Risks.";

fn calibration(auto_at: u8) -> String {
    format!(
        "Every bullet under Assumptions, Open questions and Risks must end with [confidence: 0-100; alternatives: a, b; impact: low|medium|high|irreversible; evidence: <source>], where evidence cites the file, command or document you checked. When your confidence is {auto_at} or higher, take the recommended answer and keep going; only low-confidence, high-impact items are escalated to the human, and a bullet without this bracket is escalated too."
    )
}

const EMBEDDED: [(&str, &str); 6] = [
    ("grill", include_str!("../prompts/grill.md")),
    ("spec", include_str!("../prompts/spec.md")),
    ("investigate", include_str!("../prompts/investigate.md")),
    ("implement", include_str!("../prompts/implement.md")),
    ("fix", include_str!("../prompts/fix.md")),
    ("review", include_str!("../prompts/review.md")),
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TemplateError {
    #[error("template '{0}' has no frontmatter with a version")]
    MissingVersion(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub version: u32,
    body: String,
}

fn parse(name: &str, text: &str) -> Result<Template, TemplateError> {
    let missing = || TemplateError::MissingVersion(name.to_owned());
    let rest = text.strip_prefix("---\n").ok_or_else(missing)?;
    let (front, body) = rest.split_once("\n---\n").ok_or_else(missing)?;
    let version = front
        .lines()
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim() == "version")
        .and_then(|(_, v)| v.trim().parse::<u32>().ok())
        .ok_or_else(missing)?;
    Ok(Template {
        name: name.to_owned(),
        version,
        body: body.to_owned(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Templates {
    by_name: BTreeMap<String, Template>,
}

impl Templates {
    pub fn embedded() -> Self {
        Self {
            by_name: EMBEDDED
                .iter()
                .filter_map(|(name, text)| parse(name, text).ok())
                .map(|t| (t.name.clone(), t))
                .collect(),
        }
    }

    pub fn with_override(mut self, name: &str, text: &str) -> Result<Self, TemplateError> {
        let template = parse(name, text)?;
        self.by_name.insert(name.to_owned(), template);
        Ok(self)
    }

    pub fn get(&self, name: &str) -> Option<&Template> {
        self.by_name.get(name)
    }

    pub fn has(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }
}

pub struct Vars<'a> {
    pub job: &'a str,
    pub stage: &'a str,
    pub task: &'a str,
    pub handoff_path: &'a str,
    pub artifact_dir: &'a str,
    pub spec_dir: &'a str,
    pub previous_handoff: Option<&'a str>,
    pub review_report: Option<&'a str>,
    /// Confidence at or above which the agent proceeds without escalating.
    pub auto_at: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub text: String,
    pub template: String,
    pub version: u32,
    pub prompt_sha256: String,
}

pub fn sha256_hex(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

fn substitute(body: &str, vars: &Vars<'_>) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            return out;
        };
        let value = match &after[..end] {
            "job" => Some(vars.job.to_owned()),
            "stage" => Some(vars.stage.to_owned()),
            "task" => Some(vars.task.to_owned()),
            "handoff_path" => Some(vars.handoff_path.to_owned()),
            "artifact_dir" => Some(vars.artifact_dir.to_owned()),
            "spec_dir" => Some(vars.spec_dir.to_owned()),
            "grill_round_cap" => Some(DEFAULT_ROUND_CAP.to_string()),
            "previous_handoff" => Some(vars.previous_handoff.unwrap_or("none").to_owned()),
            "review_report" => Some(vars.review_report.unwrap_or("none").to_owned()),
            _ => None,
        };
        match value {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..start + 2 + end + 2]),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

pub fn render(templates: &Templates, vars: &Vars<'_>) -> Option<Rendered> {
    let template = templates.get(vars.stage)?;
    let text = format!(
        "{PREAMBLE}\n\n{}\n\n{}\n\nWhen the work is done, write the handoff document to {} using exactly this structure (the Risks section is optional):\n\n{}",
        calibration(vars.auto_at),
        substitute(&template.body, vars).trim_end(),
        vars.handoff_path,
        handoff::template(vars.job, vars.stage)
    );
    Some(rendered(text, template))
}

fn rendered(text: String, template: &Template) -> Rendered {
    Rendered {
        prompt_sha256: sha256_hex(&text),
        text,
        template: template.name.clone(),
        version: template.version,
    }
}

impl Rendered {
    pub fn with_interruption_note(self, note: Option<&str>) -> Self {
        match note {
            None => self,
            Some(note) => {
                let text = format!(
                    "{}\n\nA previous attempt of this stage was interrupted. Read this before continuing:\n\n{note}",
                    self.text
                );
                Self {
                    prompt_sha256: sha256_hex(&text),
                    text,
                    ..self
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars<'a>(stage: &'a str) -> Vars<'a> {
        Vars {
            job: "J1",
            stage,
            task: "fix the bug",
            handoff_path: "/a/handoff.md",
            artifact_dir: "/a",
            spec_dir: "docs/specs/J1",
            previous_handoff: None,
            review_report: None,
            auto_at: 80,
        }
    }

    fn render_stage(stage: &str) -> Option<Rendered> {
        render(&Templates::embedded(), &vars(stage))
    }

    #[test]
    fn every_pipeline_stage_prompt_starts_with_the_autonomy_preamble() {
        for stage in ["grill", "spec", "investigate", "implement", "fix", "review"] {
            let prompt = render_stage(stage);
            assert!(
                prompt.as_ref().is_some_and(|p| p
                    .text
                    .starts_with("You are running unattended. Never ask the human")),
                "{stage}"
            );
        }
    }

    #[test]
    fn the_prompt_carries_task_handoff_location_and_structure() {
        let prompt = render_stage("implement")
            .map(|p| p.text)
            .unwrap_or_default();
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("/a/handoff.md"));
        assert!(prompt.contains("## Instructions for the next stage"));
        assert!(!prompt.contains("{{"));
    }

    #[test]
    fn the_fix_prompt_embeds_the_review_report() {
        let mut v = vars("fix");
        v.review_report = Some("[blocking] null deref");
        let prompt = render(&Templates::embedded(), &v).map(|p| p.text);
        assert!(prompt.is_some_and(|p| p.contains("[blocking] null deref")));
    }

    #[test]
    fn user_text_that_looks_like_a_placeholder_is_not_expanded() {
        let mut v = vars("implement");
        v.task = "mention {{handoff_path}} literally";
        let prompt = render(&Templates::embedded(), &v).map(|p| p.text);
        assert!(prompt.is_some_and(|p| p.contains("mention {{handoff_path}} literally")));
    }

    #[test]
    fn the_rendering_records_template_name_version_and_the_hash_of_the_text() {
        let prompt = render_stage("review");
        assert_eq!(
            prompt.as_ref().map(|p| (p.template.as_str(), p.version)),
            Some(("review", 2))
        );
        assert_eq!(
            prompt.as_ref().map(|p| p.prompt_sha256.clone()),
            prompt.as_ref().map(|p| sha256_hex(&p.text))
        );
    }

    #[test]
    fn an_override_replaces_the_embedded_template_and_bumps_the_recorded_version() {
        let templates = Templates::embedded()
            .with_override("implement", "---\nversion: 7\n---\nCustom for {{task}}\n");
        let prompt = templates.map(|t| render(&t, &vars("implement")));
        let prompt = prompt.ok().flatten();
        assert_eq!(prompt.as_ref().map(|p| p.version), Some(7));
        assert!(prompt.is_some_and(|p| p.text.contains("Custom for fix the bug")));
    }

    #[test]
    fn an_override_without_a_version_is_rejected() {
        assert_eq!(
            Templates::embedded().with_override("fix", "no frontmatter"),
            Err(TemplateError::MissingVersion("fix".into()))
        );
    }

    #[test]
    fn an_unknown_stage_has_no_template() {
        assert_eq!(render_stage("ship"), None);
    }

    #[test]
    fn an_interruption_note_changes_the_hash_and_is_appended() {
        let base = render_stage("implement");
        let noted = base
            .clone()
            .map(|p| p.with_interruption_note(Some("# Interruption note")));
        assert_eq!(
            base.clone().map(|p| p.with_interruption_note(None)),
            base.clone()
        );
        assert!(noted
            .as_ref()
            .is_some_and(|p| p.text.ends_with("# Interruption note")
                && p.text
                    .contains("A previous attempt of this stage was interrupted")));
        assert_ne!(
            noted.map(|p| p.prompt_sha256),
            base.map(|p| p.prompt_sha256)
        );
    }

    #[test]
    fn the_prompt_asks_for_calibrated_metadata_with_the_configured_threshold() {
        let mut v = vars("grill");
        v.auto_at = 73;
        let prompt = render(&Templates::embedded(), &v)
            .map(|p| p.text)
            .unwrap_or_default();
        assert!(prompt.contains("[confidence: 0-100; alternatives: a, b; impact: low|medium|high|irreversible; evidence: <source>]"));
        assert!(prompt.contains("confidence is 73 or higher"));
    }
}
