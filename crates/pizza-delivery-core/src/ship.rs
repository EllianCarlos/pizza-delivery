use crate::handoff;
use crate::triage::Feedback;

pub const SENSITIVE_LABEL: &str = "infra-change";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffPolicy {
    pub sensitive: Vec<String>,
}

fn is_sensitive(path: &str) -> bool {
    let path = path.trim_start_matches("./");
    let name = path.rsplit('/').next().unwrap_or(path);
    path.starts_with(".github/workflows/")
        || path.starts_with(".githooks/")
        || path.starts_with(".husky/")
        || path.starts_with(".git/hooks/")
        || name == ".envrc"
        || name == "flake.nix"
        || name == "flake.lock"
        || name == "devenv.nix"
        || name == "devenv.yaml"
        || name == "devenv.lock"
        || name == ".pre-commit-config.yaml"
        || (name.starts_with("install") && (name.ends_with(".sh") || !name.contains('.')))
}

impl DiffPolicy {
    pub fn evaluate(files: &[String]) -> Self {
        Self {
            sensitive: files.iter().filter(|f| is_sensitive(f)).cloned().collect(),
        }
    }

    pub fn is_sensitive(&self) -> bool {
        !self.sensitive.is_empty()
    }

    pub fn labels(&self) -> Vec<&'static str> {
        if self.is_sensitive() {
            vec![SENSITIVE_LABEL]
        } else {
            vec![]
        }
    }

    pub fn result(&self) -> String {
        if self.is_sensitive() {
            format!("sensitive: {}", self.sensitive.join(", "))
        } else {
            "clean".to_owned()
        }
    }

    pub fn risk(&self) -> Option<Feedback> {
        self.is_sensitive().then(|| Feedback {
            kind: "risk".to_owned(),
            severity: "high".to_owned(),
            text: format!(
                "The change touches infrastructure files: {}",
                self.sensitive.join(", ")
            ),
            confidence: None,
            alternatives: vec![],
            evidence: None,
            impact: Some("pushed as a labelled draft pull request for review".to_owned()),
        })
    }
}

const BODY_SECTIONS: [&str; 5] = [
    "goal",
    "what was done",
    "decisions made",
    "assumptions",
    "open questions",
];

pub fn pr_title(job: &str, prompt: &str) -> String {
    let first = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let short: String = first.trim().chars().take(70).collect();
    format!("pizza-delivery {job}: {short}")
}

pub fn render_pr_body(
    job: &str,
    prompt: &str,
    handoffs: &[(String, String)],
    policy: &DiffPolicy,
) -> String {
    let mut out = format!("Job `{job}`\n\n> {}\n", prompt.trim().replace('\n', "\n> "));
    if policy.is_sensitive() {
        out.push_str("\n## Infrastructure changes\n");
        for f in &policy.sensitive {
            out.push_str(&format!("- `{f}`\n"));
        }
    }
    for (stage, text) in handoffs {
        let Some(body) = handoff::body(text) else {
            continue;
        };
        out.push_str(&format!("\n## {stage}\n"));
        let found = handoff::sections(body);
        for wanted in BODY_SECTIONS {
            if let Some((_, content)) = found.iter().find(|(title, _)| title == wanted) {
                let content = content.trim();
                if !content.is_empty() && content != "none" {
                    out.push_str(&format!("\n**{wanted}**\n\n{content}\n"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn ordinary_source_changes_are_clean() {
        let p = DiffPolicy::evaluate(&files(&["src/main.rs", "README.md", "docs/install.md"]));
        assert!(!p.is_sensitive());
        assert!(p.labels().is_empty());
        assert_eq!(p.result(), "clean");
        assert_eq!(p.risk(), None);
    }

    #[test]
    fn infrastructure_files_are_sensitive() {
        for path in [
            ".github/workflows/ci.yml",
            ".githooks/pre-commit",
            ".husky/pre-push",
            ".envrc",
            "sub/.envrc",
            "devenv.nix",
            "devenv.yaml",
            "devenv.lock",
            "flake.nix",
            "flake.lock",
            "install.sh",
            "scripts/install-deps.sh",
        ] {
            let p = DiffPolicy::evaluate(&files(&[path]));
            assert!(p.is_sensitive(), "{path} should be sensitive");
        }
    }

    #[test]
    fn a_sensitive_change_is_labelled_and_raises_a_high_severity_risk() {
        let p = DiffPolicy::evaluate(&files(&["src/a.rs", "flake.nix"]));
        assert_eq!(p.sensitive, vec!["flake.nix"]);
        assert_eq!(p.labels(), vec![SENSITIVE_LABEL]);
        assert_eq!(p.result(), "sensitive: flake.nix");
        let risk = p.risk();
        assert_eq!(risk.as_ref().map(|r| r.kind.as_str()), Some("risk"));
        assert_eq!(risk.as_ref().map(|r| r.severity.as_str()), Some("high"));
        assert!(risk.is_some_and(|r| r.text.contains("flake.nix")));
    }

    const HANDOFF: &str = "---\njob: J\nstage: implement\n---\n\n## Goal\nadd login\n\n## What was done\nwrote it\n\n## Decisions made\nnone\n\n## Assumptions\nuses sqlite\n\n## Open questions\nnone\n\n## Files touched\na.rs\n\n## Instructions for the next stage\nreview\n";

    #[test]
    fn the_pr_body_is_rendered_from_the_handoffs() {
        let body = render_pr_body(
            "J1",
            "add login",
            &[("implement".to_owned(), HANDOFF.to_owned())],
            &DiffPolicy::evaluate(&[]),
        );
        assert!(body.contains("J1"));
        assert!(body.contains("## implement"));
        assert!(body.contains("wrote it"));
        assert!(body.contains("uses sqlite"));
        assert!(!body.contains("review"));
        assert!(!body.contains("Infrastructure changes"));
    }

    #[test]
    fn a_sensitive_pr_body_lists_the_infrastructure_files() {
        let body = render_pr_body(
            "J1",
            "p",
            &[],
            &DiffPolicy::evaluate(&files(&[".github/workflows/ci.yml"])),
        );
        assert!(body.contains("Infrastructure changes"));
        assert!(body.contains(".github/workflows/ci.yml"));
    }

    #[test]
    fn the_title_names_the_job_and_truncates_the_prompt() {
        let title = pr_title("J1", &format!("{}\nsecond", "x".repeat(200)));
        assert!(title.starts_with("pizza-delivery J1: xxx"));
        assert!(title.len() < 120);
    }
}
