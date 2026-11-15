use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use crate::lock::acquire;
use anyhow::{anyhow, Context, Result};

pub trait Git: Send + Sync {
    fn ensure_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<()>;
    /// The head branch of an open, same-repository pull request, when
    /// `pr_url` names one; `None` for closed, forked or unreadable PRs.
    fn pr_branch(&self, _repo: &Path, _pr_url: &str) -> Result<Option<String>> {
        Ok(None)
    }
    /// A worktree on the pull request's own branch (fetched if it exists
    /// only on the remote), so follow-up work lands on that PR.
    fn ensure_pr_worktree(
        &self,
        _repo: &Path,
        _worktree: &Path,
        _local: &str,
        _remote_branch: &str,
    ) -> Result<()> {
        Err(anyhow!("ensure_pr_worktree is not supported"))
    }
    fn commit_wip(&self, worktree: &Path, message: &str) -> Result<Option<String>>;
    fn is_dirty(&self, worktree: &Path) -> Result<bool>;
    fn is_pushed(&self, worktree: &Path) -> Result<bool>;
    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<()>;
    fn changed_files(&self, _repo: &Path, _worktree: &Path) -> Result<Vec<String>> {
        Err(anyhow!("changed_files is not supported"))
    }
    fn push(&self, _worktree: &Path, _remote: &str, _branch: &str) -> Result<String> {
        Err(anyhow!("push is not supported"))
    }
    fn open_draft_pr(
        &self,
        _worktree: &Path,
        _title: &str,
        _body: &str,
        _labels: &[&str],
    ) -> Result<String> {
        Err(anyhow!("open_draft_pr is not supported"))
    }
    /// Whether the pull request may be merged now: CI checks pass and the
    /// mergeable state is clean. Waits for pending checks.
    fn merge_readiness(&self, _worktree: &Path, _pr_url: &str) -> Result<MergeReadiness> {
        Err(anyhow!("merge_readiness is not supported"))
    }
    /// What failed in the pull request's CI, for a fix round.
    fn ci_failure(&self, _worktree: &Path, _pr_url: &str) -> Result<CiFailure> {
        Err(anyhow!("ci_failure is not supported"))
    }
    /// Marks the pull request ready for review and merges it with a merge
    /// commit.
    fn merge_pr(&self, _worktree: &Path, _pr_url: &str) -> Result<()> {
        Err(anyhow!("merge_pr is not supported"))
    }
}

/// The failing checks of a pull request and the tail of their logs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CiFailure {
    pub checks: Vec<String>,
    /// Every failing check also fails on the base branch, so a fix on the
    /// PR cannot help.
    pub base_also_failing: bool,
    pub log: String,
}

const CI_LOG_CHARS: usize = 6000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeReadiness {
    Ready,
    /// Mergeable, but the repository reported no CI checks at all.
    ReadyWithoutChecks,
    Blocked(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PrPoll {
    Ready,
    NoChecks,
    Pending(String),
    Blocked(String),
}

/// Classifies `gh pr view --json mergeable,mergeStateStatus,statusCheckRollup`.
fn classify_pr_view(json: &str) -> Result<PrPoll> {
    let view: serde_json::Value = serde_json::from_str(json).context("parse gh pr view")?;
    let text = |v: &serde_json::Value, key: &str| {
        v.get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned()
    };
    match text(&view, "mergeable").as_str() {
        "CONFLICTING" => {
            return Ok(PrPoll::Blocked(
                "merge conflicts with the base branch".into(),
            ))
        }
        "MERGEABLE" => {}
        _ => {
            return Ok(PrPoll::Pending(
                "mergeability is still being computed".into(),
            ))
        }
    }
    let mut failing = Vec::new();
    let mut pending = Vec::new();
    let checks = view
        .get("statusCheckRollup")
        .and_then(serde_json::Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for check in checks {
        let name = [text(check, "name"), text(check, "context")]
            .into_iter()
            .find(|n| !n.is_empty())
            .unwrap_or_else(|| "check".to_owned());
        if check.get("status").is_some() {
            if text(check, "status") != "COMPLETED" {
                pending.push(name);
            } else if !matches!(
                text(check, "conclusion").as_str(),
                "SUCCESS" | "NEUTRAL" | "SKIPPED"
            ) {
                failing.push(name);
            }
        } else {
            match text(check, "state").as_str() {
                "SUCCESS" => {}
                "PENDING" | "EXPECTED" => pending.push(name),
                _ => failing.push(name),
            }
        }
    }
    if !failing.is_empty() {
        return Ok(PrPoll::Blocked(format!(
            "checks failing: {}",
            failing.join(", ")
        )));
    }
    if !pending.is_empty() {
        return Ok(PrPoll::Pending(format!(
            "checks pending: {}",
            pending.join(", ")
        )));
    }
    match text(&view, "mergeStateStatus").as_str() {
        "CLEAN" | "DRAFT" | "HAS_HOOKS" if checks.is_empty() => Ok(PrPoll::NoChecks),
        "CLEAN" | "DRAFT" | "HAS_HOOKS" => Ok(PrPoll::Ready),
        "UNKNOWN" | "" => Ok(PrPoll::Pending("merge state is unknown".into())),
        other => Ok(PrPoll::Blocked(format!(
            "merge state is {} rather than clean",
            other.to_lowercase()
        ))),
    }
}

const READINESS_POLL: std::time::Duration = std::time::Duration::from_secs(15);
const READINESS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1200);
/// A PR with no checks at all is only treated as check-free once CI has had
/// this long to register its runs.
const NO_CHECKS_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

pub struct SystemGit;

fn run(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("run git")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn default_branch(repo: &Path) -> Result<String> {
    run(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .or_else(|_| run(repo, &["symbolic-ref", "--short", "HEAD"]))
}

impl Git for SystemGit {
    fn ensure_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<()> {
        if worktree.join(".git").exists() {
            return Ok(());
        }
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).context("create worktree parent")?;
        }
        let start = match base {
            Some(base)
                if run(
                    repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/heads/{base}"),
                    ],
                )
                .is_ok() =>
            {
                base.to_owned()
            }
            _ => default_branch(repo)?,
        };
        let path = worktree.to_string_lossy();
        run(repo, &["worktree", "add", "-b", branch, &path, &start]).map(|_| ())
    }

    fn pr_branch(&self, repo: &Path, pr_url: &str) -> Result<Option<String>> {
        let origin = run(repo, &["remote", "get-url", "origin"])?;
        let linked = pizza_delivery_core::triage::github_slugs(pr_url);
        if pizza_delivery_core::triage::remote_slug(&origin).as_ref() != linked.first() {
            return Ok(None);
        }
        let out = gh(
            repo,
            &[
                "pr",
                "view",
                pr_url,
                "--json",
                "headRefName,isCrossRepository,state",
                "--jq",
                r#"select(.state == "OPEN" and (.isCrossRepository | not)) | .headRefName"#,
            ],
        )?;
        Ok(Some(out).filter(|b| !b.is_empty()))
    }

    fn ensure_pr_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        local: &str,
        remote_branch: &str,
    ) -> Result<()> {
        if worktree.join(".git").exists() {
            return Ok(());
        }
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).context("create worktree parent")?;
        }
        // A fresh local branch tracking the PR's branch: the PR branch itself
        // may be checked out by an earlier job's worktree.
        run(repo, &["fetch", "origin", remote_branch])?;
        let path = worktree.to_string_lossy();
        run(
            repo,
            &[
                "worktree",
                "add",
                "-b",
                local,
                &path,
                &format!("origin/{remote_branch}"),
            ],
        )
        .map(|_| ())
    }

    fn commit_wip(&self, worktree: &Path, message: &str) -> Result<Option<String>> {
        if !self.is_dirty(worktree)? {
            return Ok(None);
        }
        // Agent harness state (crash locks, session claims) is never work.
        let mut add = vec!["add", "-A", "--", "."];
        let excludes: Vec<String> = HARNESS_STATE
            .iter()
            .map(|dir| format!(":(exclude){dir}"))
            .collect();
        add.extend(excludes.iter().map(String::as_str));
        run(worktree, &add)?;
        if run(worktree, &["diff", "--cached", "--name-only"])?.is_empty() {
            return Ok(None);
        }
        run(
            worktree,
            &[
                "-c",
                "user.name=pizza-delivery",
                "-c",
                "user.email=pizza-delivery@localhost",
                "commit",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                message,
            ],
        )?;
        run(worktree, &["rev-parse", "--short", "HEAD"]).map(Some)
    }

    fn is_dirty(&self, worktree: &Path) -> Result<bool> {
        Ok(!run(worktree, &["status", "--porcelain"])?.is_empty())
    }

    fn is_pushed(&self, worktree: &Path) -> Result<bool> {
        if run(worktree, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_err() {
            return Ok(false);
        }
        Ok(run(worktree, &["rev-list", "--count", "@{upstream}..HEAD"])? == "0")
    }

    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<()> {
        let path = worktree.to_string_lossy();
        run(repo, &["worktree", "remove", &path]).map(|_| ())
    }

    fn changed_files(&self, repo: &Path, worktree: &Path) -> Result<Vec<String>> {
        let base = default_branch(repo)?;
        let range = format!("{base}...HEAD");
        Ok(run(worktree, &["diff", "--name-only", &range])?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    fn push(&self, worktree: &Path, remote: &str, branch: &str) -> Result<String> {
        // Only commits the remote lacks: state already on a PR branch must
        // not block later jobs from pushing their own work.
        let pending = run(
            worktree,
            &[
                "log",
                "--name-only",
                "--format=",
                "HEAD",
                "--not",
                &format!("--remotes={remote}"),
            ],
        )?;
        let files: Vec<String> = pending.lines().map(str::to_owned).collect();
        let state = harness_state_files(&files);
        if !state.is_empty() {
            return Err(anyhow!(
                "refusing to push agent harness state: {}",
                state.join(", ")
            ));
        }
        run(worktree, &["push", "--set-upstream", remote, branch])?;
        Ok(format!("{remote}/{branch}"))
    }

    fn open_draft_pr(
        &self,
        worktree: &Path,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<String> {
        let mut args = vec!["pr", "create", "--draft", "--title", title, "--body", body];
        // A job on an existing PR pushes its own local branch to the PR's
        // branch under another name; gh only finds the remote branch by name.
        let head = push_target(worktree);
        if let Some(head) = head.as_deref() {
            args.extend(["--head", head]);
        }
        for label in labels {
            let _ = gh(worktree, &["label", "create", label, "--force"]);
            args.extend(["--label", label]);
        }
        let out = match gh(worktree, &args) {
            Ok(out) => out,
            // A repeated ship (a retried stage, a restarted daemon) finds the
            // PR it opened before; reuse it instead of failing the job.
            Err(e) => return existing_pr_url(&format!("{e:#}")).ok_or(e),
        };
        out.lines()
            .last()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("gh pr create printed no URL"))
    }

    fn merge_readiness(&self, worktree: &Path, pr_url: &str) -> Result<MergeReadiness> {
        let started = std::time::Instant::now();
        loop {
            let json = gh(
                worktree,
                &[
                    "pr",
                    "view",
                    pr_url,
                    "--json",
                    "mergeable,mergeStateStatus,statusCheckRollup",
                ],
            )?;
            let waiting = match classify_pr_view(&json)? {
                PrPoll::Ready => return Ok(MergeReadiness::Ready),
                PrPoll::Blocked(reason) => return Ok(MergeReadiness::Blocked(reason)),
                PrPoll::NoChecks if started.elapsed() >= NO_CHECKS_GRACE => {
                    return Ok(MergeReadiness::ReadyWithoutChecks)
                }
                PrPoll::NoChecks => "no checks have reported yet".to_owned(),
                PrPoll::Pending(reason) => reason,
            };
            if started.elapsed() >= READINESS_TIMEOUT {
                return Ok(MergeReadiness::Blocked(format!(
                    "{waiting} after {}s",
                    READINESS_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(READINESS_POLL);
        }
    }

    fn ci_failure(&self, worktree: &Path, pr_url: &str) -> Result<CiFailure> {
        let checks = gh(
            worktree,
            &[
                "pr",
                "checks",
                pr_url,
                "--json",
                "name,bucket",
                "--jq",
                r#".[] | select(.bucket == "fail") | .name"#,
            ],
        )?;
        let checks: Vec<String> = checks.lines().map(str::to_owned).collect();
        let base = gh(
            worktree,
            &[
                "pr",
                "view",
                pr_url,
                "--json",
                "baseRefName",
                "--jq",
                ".baseRefName",
            ],
        )?;
        let on_base = gh(
            worktree,
            &[
                "api",
                &format!("repos/{{owner}}/{{repo}}/commits/{base}/check-runs"),
                "--jq",
                r#".check_runs[] | select(.conclusion == "failure") | .name"#,
            ],
        )
        .unwrap_or_default();
        let base_failing: Vec<&str> = on_base.lines().collect();
        let base_also_failing =
            !checks.is_empty() && checks.iter().all(|c| base_failing.contains(&c.as_str()));
        let branch = run(worktree, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let log = gh(
            worktree,
            &[
                "run",
                "list",
                "--branch",
                &branch,
                "--status",
                "failure",
                "--limit",
                "1",
                "--json",
                "databaseId",
                "--jq",
                ".[0].databaseId",
            ],
        )
        .ok()
        .filter(|id| !id.is_empty() && id != "null")
        .and_then(|id| gh(worktree, &["run", "view", &id, "--log-failed"]).ok())
        .unwrap_or_default();
        let skip = log.chars().count().saturating_sub(CI_LOG_CHARS);
        Ok(CiFailure {
            checks,
            base_also_failing,
            log: log.chars().skip(skip).collect(),
        })
    }

    fn merge_pr(&self, worktree: &Path, pr_url: &str) -> Result<()> {
        gh(worktree, &["pr", "ready", pr_url])?;
        gh(worktree, &["pr", "merge", pr_url, "--merge"])?;
        Ok(())
    }
}

/// Directories agent harnesses write into the worktree; never committed or shipped.
pub const HARNESS_STATE: &[&str] = &[".opencode"];

/// The changed files that are harness state and must not be pushed.
pub fn harness_state_files(files: &[String]) -> Vec<&str> {
    files
        .iter()
        .map(String::as_str)
        .filter(|f| {
            HARNESS_STATE
                .iter()
                .any(|dir| f == dir || f.starts_with(&format!("{dir}/")))
        })
        .collect()
}

/// The remote branch the worktree's branch pushes to, from its upstream.
fn push_target(worktree: &Path) -> Option<String> {
    let upstream = run(worktree, &["rev-parse", "--abbrev-ref", "@{upstream}"]).ok()?;
    upstream
        .split_once('/')
        .map(|(_, branch)| branch.to_owned())
}

/// The URL of the PR that `gh pr create` refused to duplicate, if that is why it failed.
fn existing_pr_url(error: &str) -> Option<String> {
    if !error.contains("already exists") {
        return None;
    }
    error
        .lines()
        .map(str::trim)
        .rfind(|l| l.starts_with("https://") && l.contains("/pull/"))
        .map(str::to_owned)
}

fn gh(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("gh")
        .current_dir(dir)
        .args(args)
        .output()
        .context("run gh")?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        // Name the subcommand only: the arguments may hold a whole PR body.
        let command: Vec<&str> = args.iter().take(2).copied().collect();
        Err(anyhow!(
            "gh {} failed: {}",
            command.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

pub struct LockedGit<G> {
    inner: G,
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

impl<G> LockedGit<G> {
    pub fn new(inner: G) -> Self {
        Self {
            inner,
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn serialized<T>(&self, repo: &Path, f: impl FnOnce(&G) -> Result<T>) -> Result<T> {
        let lock = {
            let mut locks = acquire(&self.locks, "locks")?;
            Arc::clone(locks.entry(repo.to_owned()).or_default())
        };
        let _held = acquire(&lock, "lock")?;
        f(&self.inner)
    }
}

impl<G: Git> Git for LockedGit<G> {
    fn ensure_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        branch: &str,
        base: Option<&str>,
    ) -> Result<()> {
        self.serialized(repo, |g| g.ensure_worktree(repo, worktree, branch, base))
    }

    fn commit_wip(&self, worktree: &Path, message: &str) -> Result<Option<String>> {
        self.inner.commit_wip(worktree, message)
    }

    fn is_dirty(&self, worktree: &Path) -> Result<bool> {
        self.inner.is_dirty(worktree)
    }

    fn is_pushed(&self, worktree: &Path) -> Result<bool> {
        self.inner.is_pushed(worktree)
    }

    fn remove_worktree(&self, repo: &Path, worktree: &Path) -> Result<()> {
        self.serialized(repo, |g| g.remove_worktree(repo, worktree))
    }

    fn changed_files(&self, repo: &Path, worktree: &Path) -> Result<Vec<String>> {
        self.inner.changed_files(repo, worktree)
    }

    fn pr_branch(&self, repo: &Path, pr_url: &str) -> Result<Option<String>> {
        self.inner.pr_branch(repo, pr_url)
    }

    fn ci_failure(&self, worktree: &Path, pr_url: &str) -> Result<CiFailure> {
        self.inner.ci_failure(worktree, pr_url)
    }

    fn ensure_pr_worktree(
        &self,
        repo: &Path,
        worktree: &Path,
        local: &str,
        remote_branch: &str,
    ) -> Result<()> {
        self.serialized(repo, |g| {
            g.ensure_pr_worktree(repo, worktree, local, remote_branch)
        })
    }

    fn push(&self, worktree: &Path, remote: &str, branch: &str) -> Result<String> {
        self.inner.push(worktree, remote, branch)
    }

    fn open_draft_pr(
        &self,
        worktree: &Path,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<String> {
        self.inner.open_draft_pr(worktree, title, body, labels)
    }

    fn merge_readiness(&self, worktree: &Path, pr_url: &str) -> Result<MergeReadiness> {
        self.inner.merge_readiness(worktree, pr_url)
    }

    fn merge_pr(&self, worktree: &Path, pr_url: &str) -> Result<()> {
        self.inner.merge_pr(worktree, pr_url)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn poll(json: &str) -> PrPoll {
        classify_pr_view(json).unwrap_or_else(|e| PrPoll::Blocked(format!("invalid json: {e}")))
    }

    #[test]
    fn a_duplicate_pr_error_yields_the_existing_pr() {
        let err = "gh pr create failed: a pull request for branch \"pizza/J1\" into branch \"main\" already exists:\nhttps://github.com/o/r/pull/1";
        assert_eq!(
            existing_pr_url(err).as_deref(),
            Some("https://github.com/o/r/pull/1")
        );
        assert_eq!(existing_pr_url("gh pr create failed: no commits"), None);
    }

    #[test]
    fn a_clean_pr_with_passing_checks_is_ready() {
        let json = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[
            {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
            {"name":"lint","status":"COMPLETED","conclusion":"SKIPPED"},
            {"context":"legacy","state":"SUCCESS"}]}"#;
        assert_eq!(poll(json), PrPoll::Ready);
    }

    #[test]
    fn a_draft_pr_is_judged_on_mergeable_and_checks_alone() {
        let json = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"DRAFT","statusCheckRollup":[
            {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"}]}"#;
        assert_eq!(poll(json), PrPoll::Ready);
    }

    #[test]
    fn failing_checks_block_and_name_the_check() {
        let json = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"UNSTABLE","statusCheckRollup":[
            {"name":"ci","status":"COMPLETED","conclusion":"FAILURE"}]}"#;
        assert_eq!(poll(json), PrPoll::Blocked("checks failing: ci".into()));
    }

    #[test]
    fn running_checks_are_pending() {
        let json = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"BLOCKED","statusCheckRollup":[
            {"name":"ci","status":"IN_PROGRESS","conclusion":""}]}"#;
        assert_eq!(poll(json), PrPoll::Pending("checks pending: ci".into()));
    }

    #[test]
    fn conflicts_and_unclean_states_block_but_unknown_mergeability_waits() {
        let conflict =
            r#"{"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY","statusCheckRollup":[]}"#;
        assert!(matches!(poll(conflict), PrPoll::Blocked(_)));
        let behind = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"BEHIND","statusCheckRollup":[
            {"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"}]}"#;
        assert_eq!(
            poll(behind),
            PrPoll::Blocked("merge state is behind rather than clean".into())
        );
        let unknown =
            r#"{"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN","statusCheckRollup":[]}"#;
        assert!(matches!(poll(unknown), PrPoll::Pending(_)));
    }

    #[test]
    fn a_pr_with_no_checks_reports_no_checks_so_ci_gets_time_to_register() {
        let json = r#"{"mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","statusCheckRollup":[]}"#;
        assert_eq!(poll(json), PrPoll::NoChecks);
    }

    #[derive(Default)]
    struct Probe {
        inside: AtomicUsize,
        overlaps: AtomicUsize,
    }

    impl Git for Probe {
        fn ensure_worktree(&self, _: &Path, _: &Path, _: &str, _: Option<&str>) -> Result<()> {
            if self.inside.fetch_add(1, Ordering::SeqCst) > 0 {
                self.overlaps.fetch_add(1, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(30));
            self.inside.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
        fn commit_wip(&self, _: &Path, _: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn is_dirty(&self, _: &Path) -> Result<bool> {
            Ok(false)
        }
        fn is_pushed(&self, _: &Path) -> Result<bool> {
            Ok(false)
        }
        fn remove_worktree(&self, _: &Path, _: &Path) -> Result<()> {
            Ok(())
        }
    }

    fn overlaps_for(repos: [&str; 2]) -> usize {
        let git = LockedGit::new(Probe::default());
        std::thread::scope(|scope| {
            for repo in repos {
                let git = &git;
                scope.spawn(move || {
                    let _ = git.ensure_worktree(Path::new(repo), Path::new("/w"), "b", None);
                });
            }
        });
        git.inner.overlaps.load(Ordering::SeqCst)
    }

    #[test]
    fn administration_of_the_same_repo_never_overlaps() {
        assert_eq!(overlaps_for(["/r", "/r"]), 0);
    }

    #[test]
    fn different_repos_are_not_serialized_against_each_other() {
        assert_eq!(overlaps_for(["/r1", "/r2"]), 1);
    }

    #[test]
    fn changed_files_lists_what_the_job_branch_changed_since_the_default_branch() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let repo = dir.path().join("repo");
        std::fs::create_dir(&repo)?;
        let commit = |msg: &str| {
            run(
                &repo,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    msg,
                ],
            )
        };
        run(&repo, &["init", "-q", "-b", "main"])?;
        commit("root")?;
        let worktree = dir.path().join("wt");
        SystemGit.ensure_worktree(&repo, &worktree, "pizza/J1", None)?;
        std::fs::write(worktree.join("flake.nix"), "x")?;
        SystemGit.commit_wip(&worktree, "work")?;

        assert_eq!(
            SystemGit.changed_files(&repo, &worktree)?,
            vec!["flake.nix"]
        );
        Ok(())
    }
}

#[cfg(test)]
mod wip_tests {
    use super::*;

    type TestResult = Result<()>;

    fn repo() -> Result<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        let d = dir.path();
        run(d, &["init", "-q", "-b", "main"])?;
        run(d, &["config", "user.email", "t@t"])?;
        run(d, &["config", "user.name", "t"])?;
        std::fs::write(d.join("a.txt"), "a")?;
        run(d, &["add", "."])?;
        run(d, &["commit", "-q", "-m", "init"])?;
        Ok(dir)
    }

    #[test]
    fn wip_commit_leaves_harness_state_out() -> TestResult {
        let dir = repo()?;
        let d = dir.path();
        std::fs::create_dir_all(d.join(".opencode/goals"))?;
        std::fs::write(d.join(".opencode/goals/state.json"), "{}")?;
        std::fs::write(d.join("b.txt"), "b")?;
        assert!(SystemGit.commit_wip(d, "wip")?.is_some());
        assert_eq!(
            run(d, &["show", "--name-only", "--format=", "HEAD"])?,
            "b.txt"
        );
        Ok(())
    }

    #[test]
    fn wip_commit_of_only_harness_state_commits_nothing() -> TestResult {
        let dir = repo()?;
        let d = dir.path();
        std::fs::create_dir_all(d.join(".opencode"))?;
        std::fs::write(d.join(".opencode/x"), "x")?;
        assert_eq!(SystemGit.commit_wip(d, "wip")?, None);
        assert_eq!(run(d, &["rev-list", "--count", "HEAD"])?, "1");
        Ok(())
    }

    #[test]
    fn push_target_is_the_upstream_branch_name_not_the_local_one() -> TestResult {
        let dir = repo()?;
        let d = dir.path();
        run(d, &["checkout", "-q", "-b", "pizza/job"])?;
        run(d, &["remote", "add", "origin", "/nowhere"])?;
        run(d, &["update-ref", "refs/remotes/origin/pizza/pr", "HEAD"])?;
        run(d, &["config", "branch.pizza/job.remote", "origin"])?;
        run(
            d,
            &["config", "branch.pizza/job.merge", "refs/heads/pizza/pr"],
        )?;
        assert_eq!(push_target(d).as_deref(), Some("pizza/pr"));
        Ok(())
    }

    #[test]
    fn push_refuses_unpushed_harness_state_but_not_already_pushed() -> TestResult {
        let dir = repo()?;
        let d = dir.path();
        let bare = tempfile::tempdir()?;
        run(bare.path(), &["init", "-q", "--bare", "-b", "main"])?;
        let url = bare.path().to_string_lossy().into_owned();
        run(d, &["remote", "add", "origin", &url])?;
        std::fs::create_dir_all(d.join(".opencode"))?;
        std::fs::write(d.join(".opencode/x"), "x")?;
        run(d, &["add", "-f", "."])?;
        run(d, &["commit", "-q", "-m", "state"])?;
        assert!(SystemGit.push(d, "origin", "main").is_err());
        // Once the remote has it (an earlier mistake), new work still ships.
        run(d, &["push", "-q", "origin", "main"])?;
        std::fs::write(d.join("c.txt"), "c")?;
        run(d, &["add", "c.txt"])?;
        run(d, &["commit", "-q", "-m", "work"])?;
        assert_eq!(SystemGit.push(d, "origin", "main")?, "origin/main");
        Ok(())
    }

    #[test]
    fn removed_spec_artifacts_stay_readable() -> TestResult {
        let data = tempfile::tempdir()?;
        let wt = data.path().join("worktrees/repo/job");
        std::fs::create_dir_all(&wt)?;
        run(&wt, &["init", "-q", "-b", "main"])?;
        run(&wt, &["config", "user.email", "t@t"])?;
        run(&wt, &["config", "user.name", "t"])?;
        std::fs::create_dir_all(wt.join("docs"))?;
        std::fs::write(wt.join("docs/spec.md"), "spec body")?;
        run(&wt, &["add", "."])?;
        run(&wt, &["commit", "-q", "-m", "add"])?;
        run(&wt, &["rm", "-q", "docs/spec.md"])?;
        run(&wt, &["commit", "-q", "-m", "rm"])?;
        let got =
            crate::http::removed_worktree_file(data.path(), "worktrees/repo/job/docs/spec.md");
        assert_eq!(got.as_deref(), Some("spec body"));
        Ok(())
    }

    #[test]
    fn harness_state_files_are_detected() {
        let files = vec![
            "src/a.rs".to_owned(),
            ".opencode/goals/state.json".to_owned(),
            ".opencodeish/x".to_owned(),
        ];
        assert_eq!(harness_state_files(&files), [".opencode/goals/state.json"]);
    }
}
