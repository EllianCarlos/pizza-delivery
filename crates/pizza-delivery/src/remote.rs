use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::access::{HttpKeySource, Identity, Reject, Verifier};
use crate::config::RemoteConfig;

const CLOUDFLARE_HEADERS: [&str; 5] = [
    "cf-ray",
    "cf-connecting-ip",
    "cf-visitor",
    "cf-ipcountry",
    "cf-access-jwt-assertion",
];
const ASSERTION_HEADER: &str = "cf-access-jwt-assertion";
const RAY_HEADER: &str = "cf-ray";
const CLIENT_HEADER: &str = "x-pizza-client";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Local,
    Remote { email: String, ray: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submit {
    Allowed,
    Disabled,
    Limited,
}

struct Rejections {
    since: Option<Instant>,
    counts: BTreeMap<Reject, u32>,
}

pub struct Gate {
    verifier: Option<Verifier>,
    submits_enabled: bool,
    submit_limit: u32,
    submits: Mutex<VecDeque<Instant>>,
    rejections: Mutex<Rejections>,
    window: Duration,
}

pub fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

pub fn is_remote(headers: &[(String, String)]) -> bool {
    CLOUDFLARE_HEADERS
        .iter()
        .any(|name| header(headers, name).is_some())
}

impl Gate {
    pub fn local_only(window_ms: u64) -> Self {
        Self::build(None, true, 0, window_ms)
    }

    pub fn new(config: &RemoteConfig, window_ms: u64) -> Self {
        let verifier = Verifier::new(
            config,
            Box::new(HttpKeySource::new(config.certs_url.clone())),
        );
        Self::build(
            Some(verifier),
            config.submits_enabled,
            config.submit_limit_per_minute,
            window_ms,
        )
    }

    fn build(verifier: Option<Verifier>, enabled: bool, limit: u32, window_ms: u64) -> Self {
        Self {
            verifier,
            submits_enabled: enabled,
            submit_limit: limit,
            submits: Mutex::new(VecDeque::new()),
            rejections: Mutex::new(Rejections {
                since: None,
                counts: BTreeMap::new(),
            }),
            window: Duration::from_millis(window_ms),
        }
    }

    pub fn authenticate(
        &self,
        headers: &[(String, String)],
        now_secs: i64,
    ) -> Result<Origin, Reject> {
        if !is_remote(headers) {
            return Ok(Origin::Local);
        }
        let outcome = match &self.verifier {
            Some(verifier) => verifier.verify(header(headers, ASSERTION_HEADER), now_secs),
            None => Err(Reject::Disabled),
        };
        match outcome {
            Ok(Identity { email }) => Ok(Origin::Remote {
                email,
                ray: header(headers, RAY_HEADER).map(str::to_owned),
            }),
            Err(reject) => {
                self.record(reject);
                Err(reject)
            }
        }
    }

    fn record(&self, reject: Reject) {
        if let Ok(mut guard) = self.rejections.lock() {
            guard.since.get_or_insert_with(Instant::now);
            *guard.counts.entry(reject).or_default() += 1;
        }
    }

    pub fn take_due_rejections(&self, force: bool) -> Option<BTreeMap<Reject, u32>> {
        let mut guard = self.rejections.lock().ok()?;
        let since = guard.since?;
        if !force && since.elapsed() < self.window {
            return None;
        }
        guard.since = None;
        Some(std::mem::take(&mut guard.counts))
    }

    pub fn submit(&self) -> Submit {
        if !self.submits_enabled {
            return Submit::Disabled;
        }
        let Ok(mut recent) = self.submits.lock() else {
            return Submit::Limited;
        };
        let minute = Duration::from_secs(60);
        while recent.front().is_some_and(|at| at.elapsed() >= minute) {
            recent.pop_front();
        }
        if recent.len() as u32 >= self.submit_limit {
            return Submit::Limited;
        }
        recent.push_back(Instant::now());
        Submit::Allowed
    }
}

pub fn write_allowed(headers: &[(String, String)], remote: bool) -> bool {
    let origin = header(headers, "origin");
    if !remote && origin.is_none() {
        return true;
    }
    if header(headers, CLIENT_HEADER).is_none() {
        return false;
    }
    let Some(origin) = origin else {
        return false;
    };
    let host = header(headers, "x-forwarded-host").or_else(|| header(headers, "host"));
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    host.is_some_and(|h| h.eq_ignore_ascii_case(authority))
}
