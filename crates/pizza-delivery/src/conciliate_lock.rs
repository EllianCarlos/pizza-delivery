//! Best-effort lock so concurrent agents do not conciliate the same repo or PR
//! at once. One file per key; owner, pid and expiry live in the file; a stale
//! lock (expired or dead pid) is taken over; a busy lock is a reported refusal.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    pub key: String,
    pub owner: String,
    pub pid: u32,
    pub token: String,
    pub acquired_ms: i64,
    pub heartbeat_ms: i64,
    pub expires_ms: i64,
}

#[derive(Debug)]
pub enum Acquire {
    Held(LockGuard),
    /// Someone else holds a live lock. Reported, never waited on.
    Refused(LockInfo),
}

pub struct LockDir {
    dir: PathBuf,
    ttl_ms: i64,
    pid_alive: fn(u32) -> bool,
}

const GATE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn system_pid_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new("/proc").join(pid.to_string()).exists()
    } else {
        true
    }
}

fn file_name(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let slug: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(40)
        .collect();
    format!("{slug}-{hex}.lock")
}

fn read_info(path: &Path) -> io::Result<Option<LockInfo>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(serde_json::from_str(&text).ok()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn to_io(e: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

/// Writes `info` to a private temp file and hard-links it into place: the
/// lock file appears atomically, complete, or not at all.
fn publish_new(path: &Path, info: &LockInfo) -> io::Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", info.token.replace([':', '/'], "-")));
    fs::write(&tmp, serde_json::to_vec(info).map_err(to_io)?)?;
    let linked = fs::hard_link(&tmp, path);
    let _ = fs::remove_file(&tmp);
    linked
}

impl LockDir {
    pub fn new(dir: impl Into<PathBuf>, ttl_ms: i64) -> Self {
        Self {
            dir: dir.into(),
            ttl_ms,
            pid_alive: system_pid_alive,
        }
    }

    #[cfg(test)]
    pub fn with_pid_alive(mut self, pid_alive: fn(u32) -> bool) -> Self {
        self.pid_alive = pid_alive;
        self
    }

    fn is_stale(&self, info: &LockInfo, now_ms: i64) -> bool {
        now_ms > info.expires_ms || !(self.pid_alive)(info.pid)
    }

    pub fn acquire(&self, key: &str, owner: &str, now_ms: i64) -> io::Result<Acquire> {
        self.acquire_as(key, owner, std::process::id(), now_ms)
    }

    pub fn acquire_as(&self, key: &str, owner: &str, pid: u32, now_ms: i64) -> io::Result<Acquire> {
        fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(file_name(key));
        let info = LockInfo {
            key: key.to_owned(),
            owner: owner.to_owned(),
            pid,
            token: format!(
                "{owner}:{pid}:{now_ms}:{}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            acquired_ms: now_ms,
            heartbeat_ms: now_ms,
            expires_ms: now_ms + self.ttl_ms,
        };
        for _ in 0..20 {
            let won = match publish_new(&path, &info) {
                Ok(()) => true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    match read_info(&path)? {
                        Some(seen) if !self.is_stale(&seen, now_ms) => {
                            return Ok(Acquire::Refused(seen));
                        }
                        // Stale, or unreadable (a corrupt file counts as stale).
                        _ => self.take_over(&path, &info, now_ms)?,
                    }
                }
                Err(e) => return Err(e),
            };
            if won {
                return Ok(Acquire::Held(LockGuard {
                    path,
                    info,
                    ttl_ms: self.ttl_ms,
                    released: false,
                }));
            }
            // Lost a takeover race or the lock vanished: look again.
            if let Some(live) = read_info(&path)? {
                if !self.is_stale(&live, now_ms) {
                    return Ok(Acquire::Refused(live));
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        match read_info(&path)? {
            Some(held) => Ok(Acquire::Refused(held)),
            None => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "lock contended; try again",
            )),
        }
    }

    /// Replaces a stale lock with `mine`. Takeovers are serialized by a
    /// short-lived gate file, and staleness is re-checked under it, so a live
    /// lock is never removed and only one contender wins.
    fn take_over(&self, path: &Path, mine: &LockInfo, now_ms: i64) -> io::Result<bool> {
        let gate = path.with_extension("takeover");
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&gate)
        {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                // A crashed taker leaves its gate behind; clear an old one.
                let old = fs::metadata(&gate)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > GATE_MAX_AGE);
                if old {
                    let _ = fs::remove_file(&gate);
                }
                return Ok(false);
            }
            Err(e) => return Err(e),
        }
        let result = (|| {
            if path.exists() {
                let still_stale = match read_info(path)? {
                    Some(current) => self.is_stale(&current, now_ms),
                    None => true,
                };
                if !still_stale {
                    return Ok(false);
                }
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                }
            }
            match publish_new(path, mine) {
                Ok(()) => Ok(true),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(e) => Err(e),
            }
        })();
        let _ = fs::remove_file(&gate);
        result
    }
}

#[derive(Debug)]
pub struct LockGuard {
    path: PathBuf,
    info: LockInfo,
    ttl_ms: i64,
    released: bool,
}

impl LockGuard {
    #[cfg(test)]
    pub fn info(&self) -> &LockInfo {
        &self.info
    }

    /// Pushes the expiry forward. `Ok(false)` means the lock was lost to a
    /// takeover and the caller should stop.
    pub fn heartbeat(&mut self, now_ms: i64) -> io::Result<bool> {
        match read_info(&self.path)? {
            Some(on_disk) if on_disk.token == self.info.token => {}
            _ => return Ok(false),
        }
        self.info.heartbeat_ms = now_ms;
        self.info.expires_ms = now_ms + self.ttl_ms;
        let tmp = self.path.with_extension("beat");
        fs::write(&tmp, serde_json::to_vec(&self.info).map_err(to_io)?)?;
        fs::rename(&tmp, &self.path)?;
        Ok(true)
    }

    pub fn release(mut self) {
        self.release_inner();
    }

    fn release_inner(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Ok(Some(on_disk)) = read_info(&self.path) {
            if on_disk.token == self.info.token {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.release_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    const TTL: i64 = 1_000;

    fn dir() -> Result<tempfile::TempDir, io::Error> {
        tempfile::tempdir()
    }

    fn always_alive(_: u32) -> bool {
        true
    }

    fn never_alive(_: u32) -> bool {
        false
    }

    fn locks(d: &Path) -> LockDir {
        LockDir::new(d, TTL).with_pid_alive(always_alive)
    }

    #[test]
    fn a_free_lock_is_acquired_and_released_on_drop() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        match l.acquire("repo", "a", 0)? {
            Acquire::Held(g) => assert_eq!(g.info().owner, "a"),
            Acquire::Refused(_) => return Err(io::Error::other("should be free")),
        }
        assert!(matches!(l.acquire("repo", "b", 1)?, Acquire::Held(_)));
        Ok(())
    }

    #[test]
    fn contention_is_refused_with_the_holders_details() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        let _held = l.acquire_as("repo", "first", 4242, 0)?;
        match l.acquire_as("repo", "second", 99, 500)? {
            Acquire::Refused(info) => {
                assert_eq!(info.owner, "first");
                assert_eq!(info.pid, 4242);
                assert_eq!(info.expires_ms, TTL);
            }
            Acquire::Held(_) => return Err(io::Error::other("should be refused")),
        }
        Ok(())
    }

    #[test]
    fn different_keys_do_not_contend() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        let a = l.acquire("repo#1", "a", 0)?;
        let b = l.acquire("repo#2", "b", 0)?;
        assert!(matches!((a, b), (Acquire::Held(_), Acquire::Held(_))));
        Ok(())
    }

    #[test]
    fn an_expired_lock_is_taken_over() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        let old = l.acquire("repo", "old", 0)?;
        match l.acquire("repo", "new", TTL + 1)? {
            Acquire::Held(g) => assert_eq!(g.info().owner, "new"),
            Acquire::Refused(_) => return Err(io::Error::other("stale lock not taken over")),
        }
        drop(old);
        Ok(())
    }

    #[test]
    fn a_lock_of_a_dead_pid_is_taken_over_before_it_expires() -> io::Result<()> {
        let d = dir()?;
        let _old = locks(d.path()).acquire("repo", "old", 0)?;
        let l = LockDir::new(d.path(), TTL).with_pid_alive(never_alive);
        assert!(matches!(l.acquire("repo", "new", 10)?, Acquire::Held(_)));
        Ok(())
    }

    #[test]
    fn a_corrupt_lock_file_is_stale() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        fs::write(d.path().join(file_name("repo")), b"not json")?;
        assert!(matches!(l.acquire("repo", "a", 0)?, Acquire::Held(_)));
        Ok(())
    }

    #[test]
    fn a_heartbeat_extends_the_expiry_and_keeps_others_out() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        let Acquire::Held(mut g) = l.acquire("repo", "a", 0)? else {
            return Err(io::Error::other("free"));
        };
        assert!(g.heartbeat(900)?);
        assert_eq!(g.info().expires_ms, 900 + TTL);
        assert!(matches!(
            l.acquire("repo", "b", TTL + 1)?,
            Acquire::Refused(_)
        ));
        Ok(())
    }

    #[test]
    fn a_displaced_holder_cannot_release_or_heartbeat_the_new_lock() -> io::Result<()> {
        let d = dir()?;
        let l = locks(d.path());
        let Acquire::Held(mut old) = l.acquire("repo", "old", 0)? else {
            return Err(io::Error::other("free"));
        };
        let Acquire::Held(new) = l.acquire("repo", "new", TTL + 1)? else {
            return Err(io::Error::other("takeover"));
        };
        assert!(!old.heartbeat(TTL + 2)?);
        old.release();
        assert!(matches!(
            l.acquire("repo", "third", TTL + 3)?,
            Acquire::Refused(info) if info.owner == "new"
        ));
        drop(new);
        Ok(())
    }

    #[test]
    fn concurrent_contenders_yield_exactly_one_winner() -> io::Result<()> {
        let d = dir()?;
        let path = d.path().to_path_buf();
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                std::thread::spawn(move || {
                    let l = LockDir::new(&path, TTL).with_pid_alive(always_alive);
                    barrier.wait();
                    l.acquire_as("repo", &format!("t{i}"), 1000 + i as u32, 0)
                })
            })
            .collect();
        let mut winners = 0;
        let mut results = Vec::new();
        for h in handles {
            let r = h
                .join()
                .map_err(|_| io::Error::other("thread panicked"))??;
            if matches!(r, Acquire::Held(_)) {
                winners += 1;
            }
            results.push(r);
        }
        assert_eq!(winners, 1);
        Ok(())
    }

    #[test]
    fn concurrent_takeover_of_a_stale_lock_yields_exactly_one_winner() -> io::Result<()> {
        let d = dir()?;
        let _old = locks(d.path()).acquire_as("repo", "old", 1, 0)?;
        let path = d.path().to_path_buf();
        let n = 8;
        let barrier = Arc::new(Barrier::new(n));
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                std::thread::spawn(move || {
                    let l = LockDir::new(&path, TTL).with_pid_alive(always_alive);
                    barrier.wait();
                    l.acquire_as("repo", &format!("t{i}"), 2000 + i as u32, TTL + 1)
                })
            })
            .collect();
        let mut results = Vec::new();
        for h in handles {
            results.push(
                h.join()
                    .map_err(|_| io::Error::other("thread panicked"))??,
            );
        }
        let winners = results
            .iter()
            .filter(|r| matches!(r, Acquire::Held(_)))
            .count();
        assert_eq!(winners, 1);
        Ok(())
    }
}
