//! `pizza-delivery insights`: print the ledger patterns and, only when asked,
//! file the ones above the threshold as GitHub issues. Nothing is ever
//! implemented or merged from here; the issues go to triage.

use std::path::Path;
use std::process::Command;

use pizza_delivery_core::insights::PROPOSAL_MARKER;
use pizza_delivery_core::{
    proposals, render_insights, ExitCode, Insights, Proposal, Request, Response,
};

use crate::client::send;
use crate::client::Start;
use crate::config::Config;

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Filed { key: String, url: String },
    Duplicate { key: String },
}

#[derive(serde::Deserialize)]
struct OpenIssue {
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
}

fn gh(command: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(command)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run {}: {e}", command.display()))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(format!(
            "{} {} failed: {}",
            command.display(),
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// File each proposal as an issue labelled `needs-triage`, skipping any that an
/// open issue already covers (same marker in its body, or same title).
pub fn file_issues(
    command: &Path,
    repo: &str,
    proposals: &[Proposal],
) -> Result<Vec<Outcome>, String> {
    let listing = gh(
        command,
        &[
            "issue",
            "list",
            "--repo",
            repo,
            "--state",
            "open",
            "--limit",
            "500",
            "--json",
            "title,body",
        ],
    )?;
    let mut open: Vec<OpenIssue> =
        serde_json::from_str(&listing).map_err(|e| format!("unreadable issue list: {e}"))?;
    let mut outcomes = Vec::new();
    for p in proposals {
        let marker = format!("{PROPOSAL_MARKER}{}", p.key);
        if open
            .iter()
            .any(|i| i.title == p.title || i.body.contains(&marker))
        {
            outcomes.push(Outcome::Duplicate { key: p.key.clone() });
            continue;
        }
        let url = gh(
            command,
            &[
                "issue",
                "create",
                "--repo",
                repo,
                "--title",
                &p.title,
                "--body",
                &p.body,
                "--label",
                "needs-triage",
            ],
        )?;
        open.push(OpenIssue {
            title: p.title.clone(),
            body: p.body.clone(),
        });
        outcomes.push(Outcome::Filed {
            key: p.key.clone(),
            url,
        });
    }
    Ok(outcomes)
}

pub fn run(config: &Config, json: bool, file: bool, threshold: Option<u32>) -> ExitCode {
    let insights: Insights = match send(config, &Request::Insights, Start::IfNeeded) {
        Ok(Response::Insights { insights }) => insights,
        Ok(Response::Error { error }) => {
            eprintln!("error: {}", error.message);
            return error.exit_code();
        }
        Ok(_) => {
            eprintln!("error: unexpected response");
            return ExitCode::General;
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::DaemonUnreachable;
        }
    };
    print!("{}", render_insights(&insights, json));
    let found = proposals(&insights, threshold.unwrap_or(config.insights_threshold));
    let file = file || config.insights_file_issues;
    if !file {
        for p in &found {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"section": "proposal", "key": p.key, "title": p.title})
                );
            } else {
                println!("proposal (not filed; pass --file-issues): {}", p.title);
            }
        }
        return ExitCode::Success;
    }
    match file_issues(&config.gh_command, &config.insights_repo, &found) {
        Ok(outcomes) => {
            for o in outcomes {
                match (o, json) {
                    (Outcome::Filed { key, url }, true) => println!(
                        "{}",
                        serde_json::json!({"section": "issue", "key": key, "url": url})
                    ),
                    (Outcome::Filed { url, .. }, false) => println!("filed {url}"),
                    (Outcome::Duplicate { key }, true) => println!(
                        "{}",
                        serde_json::json!({"section": "issue", "key": key, "skipped": "open issue exists"})
                    ),
                    (Outcome::Duplicate { key }, false) => {
                        println!("skipped {key}: an open issue already covers it")
                    }
                }
            }
            ExitCode::Success
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::General
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn proposal(key: &str) -> Proposal {
        Proposal {
            key: key.into(),
            title: format!("Insight: {key}"),
            body: format!("evidence\n<!-- {PROPOSAL_MARKER}{key} -->"),
        }
    }

    /// A fake `gh`: `issue list` prints $LIST; `issue create` appends its args to $LOG.
    fn fake_gh(dir: &Path, list: &str) -> Result<std::path::PathBuf, std::io::Error> {
        let log = dir.join("created.log");
        let script = dir.join("gh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$2\" = list ]; then cat <<'EOF'\n{list}\nEOF\nelse echo \"$@\" >> {}; echo https://github.com/o/r/issues/9; fi\n",
                log.display()
            ),
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok(script)
    }

    #[test]
    fn files_new_proposals_with_needs_triage_and_never_duplicates_open_issues(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let list = r#"[{"title":"unrelated","body":"<!-- pizza-delivery-insight:a -->"}]"#;
        let gh = fake_gh(dir.path(), list)?;
        let out = file_issues(&gh, "o/r", &[proposal("a"), proposal("b"), proposal("b")])?;
        assert_eq!(
            out,
            vec![
                Outcome::Duplicate { key: "a".into() },
                Outcome::Filed {
                    key: "b".into(),
                    url: "https://github.com/o/r/issues/9".into()
                },
                Outcome::Duplicate { key: "b".into() },
            ]
        );
        let created = std::fs::read_to_string(dir.path().join("created.log"))?;
        assert_eq!(created.matches("issue create").count(), 1);
        assert!(created.contains("--label needs-triage"), "{created}");
        assert!(created.contains("--repo o/r"), "{created}");
        Ok(())
    }

    #[test]
    fn a_missing_gh_is_an_error_not_a_panic() {
        let out = file_issues(Path::new("/nonexistent/gh"), "o/r", &[proposal("a")]);
        assert!(out.is_err());
    }
}
