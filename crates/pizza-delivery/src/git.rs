use std::path::Path;
use std::process::Command;

use anyhow::{anyhow, Context, Result};

pub trait Git: Send + Sync {
    fn ensure_worktree(&self, repo: &Path, worktree: &Path, branch: &str) -> Result<()>;
}

pub struct SystemGit;

fn run(repo: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
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
    fn ensure_worktree(&self, repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
        if worktree.join(".git").exists() {
            return Ok(());
        }
        if let Some(parent) = worktree.parent() {
            std::fs::create_dir_all(parent).context("create worktree parent")?;
        }
        let base = default_branch(repo)?;
        let path = worktree.to_string_lossy();
        run(repo, &["worktree", "add", "-b", branch, &path, &base]).map(|_| ())
    }
}
