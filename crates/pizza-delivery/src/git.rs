use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};

pub trait Git: Send + Sync {
    fn ensure_worktree(&self, repo: &Path, worktree: &Path, branch: &str) -> Result<()>;
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
}

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

    fn commit_wip(&self, worktree: &Path, message: &str) -> Result<Option<String>> {
        if !self.is_dirty(worktree)? {
            return Ok(None);
        }
        run(worktree, &["add", "-A"])?;
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
        for label in labels {
            let _ = gh(worktree, &["label", "create", label, "--force"]);
            args.extend(["--label", label]);
        }
        let out = gh(worktree, &args)?;
        out.lines()
            .last()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("gh pr create printed no URL"))
    }
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
        Err(anyhow!(
            "gh {} failed: {}",
            args.join(" "),
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
            let mut locks = self
                .locks
                .lock()
                .map_err(|_| anyhow!("git lock poisoned"))?;
            Arc::clone(locks.entry(repo.to_owned()).or_default())
        };
        let _held = lock.lock().map_err(|_| anyhow!("repo lock poisoned"))?;
        f(&self.inner)
    }
}

impl<G: Git> Git for LockedGit<G> {
    fn ensure_worktree(&self, repo: &Path, worktree: &Path, branch: &str) -> Result<()> {
        self.serialized(repo, |g| g.ensure_worktree(repo, worktree, branch))
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
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[derive(Default)]
    struct Probe {
        inside: AtomicUsize,
        overlaps: AtomicUsize,
    }

    impl Git for Probe {
        fn ensure_worktree(&self, _: &Path, _: &Path, _: &str) -> Result<()> {
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
                    let _ = git.ensure_worktree(Path::new(repo), Path::new("/w"), "b");
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
        SystemGit.ensure_worktree(&repo, &worktree, "pizza/J1")?;
        std::fs::write(worktree.join("flake.nix"), "x")?;
        SystemGit.commit_wip(&worktree, "work")?;

        assert_eq!(
            SystemGit.changed_files(&repo, &worktree)?,
            vec!["flake.nix"]
        );
        Ok(())
    }
}
