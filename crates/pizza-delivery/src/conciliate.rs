//! The conciliate step: gather facts about the open `pizza/*` PRs of a repo,
//! plan with the pure core planner, and perform only the safe mechanical
//! actions (merge the default branch into clean branches, plain push).
//! Conflicts are reported as inbox entries, never guessed at.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use pizza_delivery_core::conciliate::{plan, Action, ConflictWith, OpenPr, Plan, PlanInput};
use pizza_delivery_core::event::{Actor, EventPayload, SYSTEM_JOB};
use pizza_delivery_core::id::JobId;
use pizza_delivery_core::store::{NewEvent, Store};
use serde::Deserialize;

use crate::conciliate_lock::{Acquire, LockDir};
use crate::config::Config;
use crate::sqlite_store::SqliteStore;

/// Everything conciliate needs from git and GitHub. `SystemConciliator`
/// shells out; tests use a fake.
pub trait Conciliator: Send + Sync {
    /// A stable identity for the repo (the origin URL, else its path).
    fn repo_id(&self, repo: &Path) -> Result<String>;
    fn fetch(&self, repo: &Path) -> Result<()>;
    /// The default branch as a ref, e.g. `origin/main`.
    fn base_ref(&self, repo: &Path) -> Result<String>;
    /// Open `pizza/*` PRs targeting `base`, with heads resolved from `origin`.
    fn list_open_prs(&self, repo: &Path, base: &str) -> Result<Vec<OpenPr>>;
    fn is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> Result<bool>;
    /// Conflicted paths of merging `b` into `a`; empty when clean.
    fn merge_conflicts(&self, repo: &Path, a: &str, b: &str) -> Result<Vec<String>>;
    /// Merges `base` into the PR branch and plain-pushes it; returns the new head.
    fn merge_base_and_push(&self, repo: &Path, pr: &OpenPr, base: &str) -> Result<String>;
}

/// Where outcomes go: ledger events and inbox entries.
pub trait Recorder {
    fn record(&mut self, repo: &str, pr: &OpenPr, action: &str, detail: &str) -> Result<()>;
    /// Files an inbox entry for a conflict. `Ok(true)` when a new entry was
    /// filed, `Ok(false)` when an identical one already exists or the branch
    /// names no known job.
    fn file_conflict(&mut self, pr: &OpenPr, text: &str) -> Result<bool>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    UpToDate { pr: u64 },
    Merged { pr: u64, head: String },
    MergeFailed { pr: u64, reason: String },
    ConflictReported { pr: u64, filed: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub plan: Plan,
    pub outcomes: Vec<Outcome>,
    pub dry_run: bool,
}

pub fn gather(git: &dyn Conciliator, repo: &Path) -> Result<PlanInput> {
    git.fetch(repo)?;
    let base = git.base_ref(repo)?;
    let prs = git.list_open_prs(repo, &base)?;
    let mut input = PlanInput {
        base: base.clone(),
        prs: prs.clone(),
        ..PlanInput::default()
    };
    for pr in &prs {
        if !git.is_ancestor(repo, &base, &pr.head)? {
            input.behind.insert(pr.number);
            let files = git.merge_conflicts(repo, &pr.head, &base)?;
            if !files.is_empty() {
                input.base_conflicts.insert(pr.number, files);
            }
        }
    }
    for outer in &prs {
        for inner in &prs {
            if outer.number != inner.number && git.is_ancestor(repo, &inner.head, &outer.head)? {
                input.contains.insert((outer.number, inner.number));
            }
        }
    }
    for (i, a) in prs.iter().enumerate() {
        for b in &prs[i + 1..] {
            let related = input.contains.contains(&(a.number, b.number))
                || input.contains.contains(&(b.number, a.number));
            if related {
                continue;
            }
            let files = git.merge_conflicts(repo, &a.head, &b.head)?;
            if !files.is_empty() {
                let key = (a.number.min(b.number), a.number.max(b.number));
                input.pair_conflicts.insert(key, files);
            }
        }
    }
    Ok(input)
}

fn short(sha: &str) -> &str {
    sha.get(..8).unwrap_or(sha)
}

fn conflict_text(plan: &Plan, pr: &OpenPr, with: ConflictWith, files: &[String]) -> String {
    let (other, default_side) = match with {
        ConflictWith::Base => (
            plan.base.clone(),
            "keep the branch side of files this PR owns and main's additions elsewhere".to_owned(),
        ),
        ConflictWith::Pr(n) => (
            format!("PR #{n}"),
            format!("the earlier stack member #{n} wins"),
        ),
    };
    format!(
        "PR #{} ({}) at {} conflicts with {} in {}. Default resolution (stack side): {}. \
         Needs the conflict-resolution stage or a human; tests must pass before any push.",
        pr.number,
        pr.branch,
        short(&pr.head),
        other,
        files.join(", "),
        default_side
    )
}

/// Plans and, unless `dry_run`, performs the safe mechanical actions.
/// `on_step` runs between actions (the caller's lock heartbeat).
pub fn run(
    git: &dyn Conciliator,
    recorder: &mut dyn Recorder,
    repo: &Path,
    dry_run: bool,
    on_step: &mut dyn FnMut(),
) -> Result<Report> {
    let input = gather(git, repo)?;
    let plan = plan(&input);
    let mut outcomes = Vec::new();
    if dry_run {
        return Ok(Report {
            plan,
            outcomes,
            dry_run,
        });
    }
    let repo_id = git.repo_id(repo)?;
    for action in &plan.actions {
        on_step();
        let Some(pr) = plan.pr(action.pr()).cloned() else {
            continue;
        };
        match action {
            Action::UpToDate { pr } => outcomes.push(Outcome::UpToDate { pr: *pr }),
            Action::MergeBase { .. } => match git.merge_base_and_push(repo, &pr, &plan.base) {
                Ok(head) => {
                    recorder.record(&repo_id, &pr, "merged-base", &format!("pushed {head}"))?;
                    outcomes.push(Outcome::Merged {
                        pr: pr.number,
                        head,
                    });
                }
                Err(e) => {
                    let reason = format!("{e:#}");
                    recorder.record(&repo_id, &pr, "merge-failed", &reason)?;
                    outcomes.push(Outcome::MergeFailed {
                        pr: pr.number,
                        reason,
                    });
                }
            },
            Action::Conflict { with, files, .. } => {
                let text = conflict_text(&plan, &pr, *with, files);
                let filed = recorder.file_conflict(&pr, &text)?;
                recorder.record(&repo_id, &pr, "conflict-reported", &text)?;
                outcomes.push(Outcome::ConflictReported {
                    pr: pr.number,
                    filed,
                });
            }
        }
    }
    Ok(Report {
        plan,
        outcomes,
        dry_run,
    })
}

pub fn render(report: &Report) -> String {
    let plan = &report.plan;
    let mut out = String::new();
    out.push_str(&format!(
        "conciliate{} against {}\n",
        if report.dry_run { " (dry run)" } else { "" },
        plan.base
    ));
    if plan.stack.is_empty() {
        out.push_str("no open pizza PRs\n");
        return out;
    }
    out.push_str("stack (merge order):\n");
    for (i, n) in plan.stack.iter().enumerate() {
        if let Some(pr) = plan.pr(*n) {
            out.push_str(&format!("  {}. #{} {}\n", i + 1, pr.number, pr.branch));
        }
    }
    out.push_str("actions:\n");
    for action in &plan.actions {
        match action {
            Action::UpToDate { pr } => out.push_str(&format!("  #{pr} up-to-date\n")),
            Action::MergeBase { pr } => out.push_str(&format!(
                "  #{pr} merge {} (clean){}\n",
                plan.base,
                if report.dry_run { "" } else { ", push" }
            )),
            Action::Conflict { pr, with, files } => {
                let with = match with {
                    ConflictWith::Base => plan.base.clone(),
                    ConflictWith::Pr(n) => format!("#{n}"),
                };
                out.push_str(&format!(
                    "  #{pr} CONFLICT with {with} in {} -> inbox\n",
                    files.join(", ")
                ));
            }
        }
    }
    if !report.outcomes.is_empty() {
        out.push_str("outcomes:\n");
        for outcome in &report.outcomes {
            match outcome {
                Outcome::UpToDate { .. } => {}
                Outcome::Merged { pr, head } => {
                    out.push_str(&format!("  #{pr} merged and pushed {}\n", short(head)))
                }
                Outcome::MergeFailed { pr, reason } => {
                    out.push_str(&format!("  #{pr} merge failed: {reason}\n"))
                }
                Outcome::ConflictReported { pr, filed } => out.push_str(&format!(
                    "  #{pr} conflict reported ({})\n",
                    if *filed {
                        "inbox entry filed"
                    } else {
                        "no new inbox entry"
                    }
                )),
            }
        }
    }
    out
}

// ---- system implementation ------------------------------------------------

pub struct SystemConciliator;

fn exec(program: &str, dir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new(program)
        .current_dir(dir)
        .args(args)
        .output()
        .with_context(|| format!("run {program}"))
}

fn git_out(dir: &Path, args: &[&str]) -> Result<String> {
    let out = exec("git", dir, args)?;
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    url: String,
}

const MERGE_IDENTITY: [&str; 4] = [
    "-c",
    "user.name=pizza-delivery",
    "-c",
    "user.email=pizza-delivery@localhost",
];

impl Conciliator for SystemConciliator {
    fn repo_id(&self, repo: &Path) -> Result<String> {
        git_out(repo, &["remote", "get-url", "origin"])
            .or_else(|_| git_out(repo, &["rev-parse", "--show-toplevel"]))
    }

    fn fetch(&self, repo: &Path) -> Result<()> {
        git_out(repo, &["fetch", "--prune", "origin"]).map(|_| ())
    }

    fn base_ref(&self, repo: &Path) -> Result<String> {
        git_out(
            repo,
            &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        )
        .or_else(|_| {
            git_out(repo, &["rev-parse", "--verify", "--quiet", "origin/main"])
                .map(|_| "origin/main".to_owned())
        })
        .context("cannot determine the default branch of origin")
    }

    fn list_open_prs(&self, repo: &Path, base: &str) -> Result<Vec<OpenPr>> {
        let out = exec(
            "gh",
            repo,
            &[
                "pr",
                "list",
                "--state",
                "open",
                "--limit",
                "200",
                "--json",
                "number,headRefName,headRefOid,baseRefName,url",
            ],
        )?;
        if !out.status.success() {
            return Err(anyhow!(
                "gh pr list failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let listed: Vec<GhPr> = serde_json::from_slice(&out.stdout).context("parse gh output")?;
        let base_branch = base.strip_prefix("origin/").unwrap_or(base);
        let mut prs: Vec<OpenPr> = listed
            .into_iter()
            .filter(|p| p.head_ref_name.starts_with("pizza/") && p.base_ref_name == base_branch)
            .map(|p| {
                let head = git_out(repo, &["rev-parse", &format!("origin/{}", p.head_ref_name)])
                    .unwrap_or(p.head_ref_oid);
                OpenPr {
                    number: p.number,
                    branch: p.head_ref_name,
                    head,
                    url: p.url,
                }
            })
            .collect();
        prs.sort_by_key(|p| p.number);
        Ok(prs)
    }

    fn is_ancestor(&self, repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
        let out = exec(
            "git",
            repo,
            &["merge-base", "--is-ancestor", ancestor, descendant],
        )?;
        match out.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(anyhow!(
                "git merge-base --is-ancestor failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    fn merge_conflicts(&self, repo: &Path, a: &str, b: &str) -> Result<Vec<String>> {
        let out = exec(
            "git",
            repo,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                a,
                b,
            ],
        )?;
        match out.status.code() {
            Some(0) => Ok(Vec::new()),
            Some(1) => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut files: Vec<String> = text
                    .lines()
                    .skip(1)
                    .take_while(|l| !l.is_empty())
                    .map(str::to_owned)
                    .collect();
                files.dedup();
                if files.is_empty() {
                    files.push("(unknown paths)".to_owned());
                }
                Ok(files)
            }
            _ => Err(anyhow!(
                "git merge-tree failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
        }
    }

    fn merge_base_and_push(&self, repo: &Path, pr: &OpenPr, base: &str) -> Result<String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let worktree = std::env::temp_dir().join(format!(
            "pizza-conciliate-{}-{}-{nanos}",
            std::process::id(),
            pr.number
        ));
        let path = worktree.to_string_lossy().into_owned();
        git_out(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                &path,
                &format!("origin/{}", pr.branch),
            ],
        )?;
        let result = (|| -> Result<String> {
            let message = format!("Merge {base} into {}", pr.branch);
            let mut merge = MERGE_IDENTITY.to_vec();
            merge.extend([
                "merge",
                "--no-edit",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                &message,
                base,
            ]);
            if let Err(e) = git_out(&worktree, &merge) {
                let _ = git_out(&worktree, &["merge", "--abort"]);
                return Err(e);
            }
            // A plain push: a moved remote rejects it, nothing is forced.
            git_out(
                &worktree,
                &["push", "origin", &format!("HEAD:refs/heads/{}", pr.branch)],
            )?;
            git_out(&worktree, &["rev-parse", "HEAD"])
        })();
        let _ = git_out(repo, &["worktree", "remove", "--force", &path]);
        result
    }
}

// ---- ledger recorder ------------------------------------------------------

pub struct StoreRecorder<'a, S: Store> {
    pub store: &'a mut S,
    pub now_ms: fn() -> i64,
}

pub fn system_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl<S: Store> Recorder for StoreRecorder<'_, S> {
    fn record(&mut self, repo: &str, pr: &OpenPr, action: &str, detail: &str) -> Result<()> {
        self.store
            .append_event(NewEvent {
                id: ulid::Ulid::new().to_string(),
                job_id: JobId::new(SYSTEM_JOB),
                stage_id: None,
                ts_ms: (self.now_ms)(),
                actor: Actor::Daemon,
                payload: EventPayload::Conciliated {
                    repo: repo.to_owned(),
                    pr: pr.number,
                    branch: pr.branch.clone(),
                    action: action.to_owned(),
                    detail: detail.to_owned(),
                },
            })
            .map(|_| ())
            .map_err(|e| anyhow!("{e}"))
    }

    fn file_conflict(&mut self, pr: &OpenPr, text: &str) -> Result<bool> {
        let Some(suffix) = pr.branch.strip_prefix("pizza/") else {
            return Ok(false);
        };
        let jobs = self.store.list_jobs().map_err(|e| anyhow!("{e}"))?;
        let Some(job) = jobs.into_iter().find(|j| j.id.as_str() == suffix) else {
            return Ok(false);
        };
        let timeline = self
            .store
            .read_timeline(&job.id)
            .map_err(|e| anyhow!("{e}"))?;
        let exists = timeline.iter().any(
            |e| matches!(&e.payload, EventPayload::FeedbackRecorded { text: t, .. } if t == text),
        );
        if exists {
            return Ok(false);
        }
        self.store
            .append_event(NewEvent {
                id: ulid::Ulid::new().to_string(),
                job_id: job.id,
                stage_id: None,
                ts_ms: (self.now_ms)(),
                actor: Actor::Daemon,
                payload: EventPayload::FeedbackRecorded {
                    kind: "risk".to_owned(),
                    severity: "high".to_owned(),
                    text: text.to_owned(),
                    confidence: None,
                    alternatives: Vec::new(),
                    impact: Some("high".to_owned()),
                    evidence: Some(pr.url.clone()),
                },
            })
            .map_err(|e| anyhow!("{e}"))?;
        Ok(true)
    }
}

// ---- CLI entry ------------------------------------------------------------

struct NoRecorder;

impl Recorder for NoRecorder {
    fn record(&mut self, _: &str, _: &OpenPr, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn file_conflict(&mut self, _: &OpenPr, _: &str) -> Result<bool> {
        Ok(false)
    }
}

const LOCK_TTL_MS: i64 = 10 * 60 * 1000;

/// `pizza-delivery conciliate [--repo] [--dry-run]`. A dry run takes no lock
/// and writes nothing. A busy lock is reported and is not an error.
pub fn execute(config: &Config, repo: Option<PathBuf>, dry_run: bool) -> Result<()> {
    let repo = match repo {
        Some(r) => r,
        None => std::env::current_dir().context("current directory")?,
    };
    let sys = SystemConciliator;
    if dry_run {
        let report = run(&sys, &mut NoRecorder, &repo, true, &mut || {})?;
        print!("{}", render(&report));
        return Ok(());
    }
    let id = sys.repo_id(&repo)?;
    std::fs::create_dir_all(&config.data_dir).context("create data dir")?;
    let locks = LockDir::new(config.data_dir.join("conciliate-locks"), LOCK_TTL_MS);
    let owner = format!("conciliate@{}", std::process::id());
    let now = system_now_ms();
    let mut guard = match locks
        .acquire(&id, &owner, now)
        .context("take the conciliate lock")?
    {
        Acquire::Held(guard) => guard,
        Acquire::Refused(held) => {
            println!(
                "conciliate refused: {} is held by {} (pid {}) for another {}s; nothing was changed",
                held.key,
                held.owner,
                held.pid,
                (held.expires_ms - now).max(0) / 1000
            );
            return Ok(());
        }
    };
    let mut store = SqliteStore::open(&config.db_path()).map_err(|e| anyhow!("{e}"))?;
    let mut recorder = StoreRecorder {
        store: &mut store,
        now_ms: system_now_ms,
    };
    let report = run(&sys, &mut recorder, &repo, false, &mut || {
        let _ = guard.heartbeat(system_now_ms());
    });
    guard.release();
    print!("{}", render(&report?));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    fn pr(n: u64) -> OpenPr {
        OpenPr {
            number: n,
            branch: format!("pizza/job{n}"),
            head: format!("head{n}"),
            url: format!("https://example.test/pull/{n}"),
        }
    }

    #[derive(Default)]
    struct Fake {
        prs: Vec<OpenPr>,
        behind: BTreeSet<u64>,
        base_conflicts: BTreeMap<u64, Vec<String>>,
        contains: BTreeSet<(String, String)>,
        pair: BTreeMap<(String, String), Vec<String>>,
        failing_push: BTreeSet<u64>,
        calls: Mutex<Vec<String>>,
    }

    impl Fake {
        fn log(&self, s: String) {
            if let Ok(mut c) = self.calls.lock() {
                c.push(s);
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().map(|c| c.clone()).unwrap_or_default()
        }
        fn number_of(&self, head: &str) -> Option<u64> {
            self.prs.iter().find(|p| p.head == head).map(|p| p.number)
        }
    }

    impl Conciliator for Fake {
        fn repo_id(&self, _: &Path) -> Result<String> {
            Ok("git@example.test:o/r.git".into())
        }
        fn fetch(&self, _: &Path) -> Result<()> {
            Ok(())
        }
        fn base_ref(&self, _: &Path) -> Result<String> {
            Ok("origin/main".into())
        }
        fn list_open_prs(&self, _: &Path, _: &str) -> Result<Vec<OpenPr>> {
            Ok(self.prs.clone())
        }
        fn is_ancestor(&self, _: &Path, a: &str, d: &str) -> Result<bool> {
            if a == "origin/main" {
                let n = self.number_of(d).ok_or_else(|| anyhow!("unknown {d}"))?;
                return Ok(!self.behind.contains(&n));
            }
            Ok(self.contains.contains(&(d.to_owned(), a.to_owned())))
        }
        fn merge_conflicts(&self, _: &Path, a: &str, b: &str) -> Result<Vec<String>> {
            if b == "origin/main" {
                let n = self.number_of(a).unwrap_or(0);
                return Ok(self.base_conflicts.get(&n).cloned().unwrap_or_default());
            }
            Ok(self
                .pair
                .get(&(a.to_owned(), b.to_owned()))
                .or_else(|| self.pair.get(&(b.to_owned(), a.to_owned())))
                .cloned()
                .unwrap_or_default())
        }
        fn merge_base_and_push(&self, _: &Path, pr: &OpenPr, base: &str) -> Result<String> {
            self.log(format!("merge {} <- {base}", pr.number));
            if self.failing_push.contains(&pr.number) {
                return Err(anyhow!("push rejected"));
            }
            Ok(format!("merged{}", pr.number))
        }
    }

    #[derive(Default)]
    struct FakeRecorder {
        events: Vec<(u64, String)>,
        filed: Vec<String>,
    }

    impl Recorder for FakeRecorder {
        fn record(&mut self, _: &str, pr: &OpenPr, action: &str, _: &str) -> Result<()> {
            self.events.push((pr.number, action.to_owned()));
            Ok(())
        }
        fn file_conflict(&mut self, _: &OpenPr, text: &str) -> Result<bool> {
            if self.filed.iter().any(|t| t == text) {
                return Ok(false);
            }
            self.filed.push(text.to_owned());
            Ok(true)
        }
    }

    fn scenario() -> Fake {
        let mut f = Fake {
            prs: vec![pr(23), pr(24), pr(25)],
            ..Fake::default()
        };
        f.behind.extend([23, 24]);
        f.base_conflicts.insert(24, vec!["src/b.rs".into()]);
        f.contains.insert(("head25".into(), "head24".into()));
        f
    }

    fn go(f: &Fake, rec: &mut FakeRecorder, dry: bool) -> Result<Report> {
        run(f, rec, Path::new("/repo"), dry, &mut || {})
    }

    #[test]
    fn dry_run_plans_but_changes_and_records_nothing() -> Result<()> {
        let f = scenario();
        let mut rec = FakeRecorder::default();
        let report = go(&f, &mut rec, true)?;
        assert_eq!(report.plan.stack, vec![23, 24, 25]);
        assert!(report.outcomes.is_empty());
        assert!(f.calls().is_empty());
        assert!(rec.events.is_empty() && rec.filed.is_empty());
        let text = render(&report);
        assert!(text.contains("(dry run)"));
        assert!(text.contains("#23 merge origin/main (clean)"));
        assert!(text.contains("#24 CONFLICT with origin/main in src/b.rs -> inbox"));
        Ok(())
    }

    #[test]
    fn a_real_run_merges_only_clean_branches_and_files_conflicts() -> Result<()> {
        let f = scenario();
        let mut rec = FakeRecorder::default();
        let report = go(&f, &mut rec, false)?;
        assert_eq!(f.calls(), vec!["merge 23 <- origin/main".to_owned()]);
        assert!(report.outcomes.contains(&Outcome::Merged {
            pr: 23,
            head: "merged23".into()
        }));
        assert!(report.outcomes.contains(&Outcome::ConflictReported {
            pr: 24,
            filed: true
        }));
        assert_eq!(rec.filed.len(), 1);
        assert!(rec.filed[0].contains("src/b.rs") && rec.filed[0].contains("stack side"));
        assert_eq!(
            rec.events,
            vec![
                (23, "merged-base".to_owned()),
                (24, "conflict-reported".to_owned())
            ]
        );
        Ok(())
    }

    #[test]
    fn a_second_run_with_the_same_heads_does_not_file_a_duplicate() -> Result<()> {
        let f = scenario();
        let mut rec = FakeRecorder::default();
        go(&f, &mut rec, false)?;
        let report = go(&f, &mut rec, false)?;
        assert_eq!(rec.filed.len(), 1);
        assert!(report.outcomes.contains(&Outcome::ConflictReported {
            pr: 24,
            filed: false
        }));
        Ok(())
    }

    #[test]
    fn a_failed_merge_is_reported_and_does_not_stop_the_others() -> Result<()> {
        let mut f = scenario();
        f.behind.insert(25);
        f.failing_push.insert(23);
        let mut rec = FakeRecorder::default();
        let report = go(&f, &mut rec, false)?;
        assert!(report.outcomes.iter().any(
            |o| matches!(o, Outcome::MergeFailed { pr: 23, reason } if reason.contains("push rejected"))
        ));
        assert!(f.calls().contains(&"merge 25 <- origin/main".to_owned()));
        Ok(())
    }

    #[test]
    fn independent_prs_that_conflict_report_on_the_later_one() -> Result<()> {
        let mut f = Fake {
            prs: vec![pr(1), pr(2)],
            ..Fake::default()
        };
        f.pair
            .insert(("head1".into(), "head2".into()), vec!["x.rs".into()]);
        let mut rec = FakeRecorder::default();
        let report = go(&f, &mut rec, false)?;
        assert_eq!(
            report.outcomes.last(),
            Some(&Outcome::ConflictReported { pr: 2, filed: true })
        );
        assert!(rec.filed[0].contains("PR #1") && rec.filed[0].contains("earlier stack member"));
        Ok(())
    }

    #[test]
    fn no_prs_is_a_quiet_empty_plan() -> Result<()> {
        let f = Fake::default();
        let mut rec = FakeRecorder::default();
        let report = go(&f, &mut rec, false)?;
        assert!(render(&report).contains("no open pizza PRs"));
        Ok(())
    }

    // ---- ledger recorder against the real sqlite store --------------------

    use pizza_delivery_core::schedule::Priority;
    use pizza_delivery_core::store::NewJob;

    #[test]
    fn the_store_recorder_files_one_inbox_entry_on_the_job_and_system_events() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut store = SqliteStore::open(&dir.path().join("l.sqlite3"))?;
        let job = "01M784RHZEZ1N3WA6K4CH6B5NW";
        store.enqueue_job(NewJob {
            id: JobId::new(job),
            event_id: "e1".into(),
            prompt: "p".into(),
            repo: Some("/r".into()),
            priority: Priority::Normal,
            follow_up: None,
            options: None,
            submitted_at_ms: 1,
        })?;
        let target = OpenPr {
            number: 9,
            branch: format!("pizza/{job}"),
            head: "abcdef0123456789".into(),
            url: "u".into(),
        };
        let mut rec = StoreRecorder {
            store: &mut store,
            now_ms: || 5,
        };
        assert!(rec.file_conflict(&target, "conflict text")?);
        assert!(!rec.file_conflict(&target, "conflict text")?);
        let stranger = OpenPr {
            branch: "pizza/unknown".into(),
            ..target.clone()
        };
        assert!(!rec.file_conflict(&stranger, "conflict text")?);
        rec.record("o/r", &target, "conflict-reported", "d")?;
        let timeline = store.read_timeline(&JobId::new(job))?;
        let feedback = timeline
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::FeedbackRecorded { .. }))
            .count();
        assert_eq!(feedback, 1);
        let system = store.read_timeline(&JobId::new(SYSTEM_JOB))?;
        assert!(system.iter().any(|e| matches!(
            &e.payload,
            EventPayload::Conciliated { pr: 9, action, .. } if action == "conflict-reported"
        )));
        Ok(())
    }

    // ---- real git against a local bare remote -----------------------------

    fn git(dir: &Path, args: &[&str]) -> Result<String> {
        let mut full = vec![
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ];
        full.extend_from_slice(args);
        git_out(dir, &full)
    }

    fn commit_file(dir: &Path, name: &str, content: &str) -> Result<()> {
        std::fs::write(dir.join(name), content)?;
        git(dir, &["add", name])?;
        git(dir, &["commit", "-m", &format!("edit {name}")])?;
        Ok(())
    }

    #[test]
    fn real_git_probes_conflicts_and_ancestry_and_merges_then_plain_pushes() -> Result<()> {
        let root = tempfile::tempdir()?;
        let origin = root.path().join("origin.git");
        let work = root.path().join("work");
        std::fs::create_dir_all(&origin)?;
        std::fs::create_dir_all(&work)?;
        git(&origin, &["init", "--bare"])?;
        git(&work, &["init"])?;
        git(
            &work,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )?;
        commit_file(&work, "a.txt", "one\n")?;
        commit_file(&work, "b.txt", "one\n")?;
        git(&work, &["push", "origin", "main"])?;
        // pizza/clean touches c.txt; pizza/clash edits a.txt; main later edits a.txt.
        git(&work, &["checkout", "-b", "pizza/clean", "main"])?;
        commit_file(&work, "c.txt", "c\n")?;
        git(&work, &["push", "origin", "pizza/clean"])?;
        git(&work, &["checkout", "-b", "pizza/clash", "main"])?;
        commit_file(&work, "a.txt", "clash\n")?;
        git(&work, &["push", "origin", "pizza/clash"])?;
        git(&work, &["checkout", "main"])?;
        commit_file(&work, "a.txt", "main\n")?;
        git(&work, &["push", "origin", "main"])?;
        git(&work, &["fetch", "origin"])?;

        let sys = SystemConciliator;
        let base = "origin/main";
        let clean = git(&work, &["rev-parse", "origin/pizza/clean"])?;
        let clash = git(&work, &["rev-parse", "origin/pizza/clash"])?;

        assert!(!sys.is_ancestor(&work, base, &clean)?);
        assert!(sys.is_ancestor(&work, "origin/main~1", &clean)?);
        assert_eq!(
            sys.merge_conflicts(&work, &clean, base)?,
            Vec::<String>::new()
        );
        assert_eq!(
            sys.merge_conflicts(&work, &clash, base)?,
            vec!["a.txt".to_owned()]
        );

        let pr = OpenPr {
            number: 1,
            branch: "pizza/clean".into(),
            head: clean.clone(),
            url: "u".into(),
        };
        let pushed = sys.merge_base_and_push(&work, &pr, base)?;
        let remote_head = git(&origin, &["rev-parse", "refs/heads/pizza/clean"])?;
        assert_eq!(pushed, remote_head);
        // The push was a fast-forward: the old head is an ancestor of the new one.
        git(
            &origin,
            &["merge-base", "--is-ancestor", &clean, &remote_head],
        )?;
        git(
            &origin,
            &[
                "merge-base",
                "--is-ancestor",
                "refs/heads/main",
                &remote_head,
            ],
        )?;

        // A conflicting merge fails cleanly and leaves the remote untouched.
        let bad = OpenPr {
            number: 2,
            branch: "pizza/clash".into(),
            head: clash.clone(),
            url: "u".into(),
        };
        assert!(sys.merge_base_and_push(&work, &bad, base).is_err());
        assert_eq!(
            git(&origin, &["rev-parse", "refs/heads/pizza/clash"])?,
            clash
        );
        Ok(())
    }
}
