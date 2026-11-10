use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use pizza_delivery_core::event::{Event, EventPayload};
use pizza_delivery_core::notify::{pr_url_of, Dispatcher, Notification, Notifier, NotifyError};
use pizza_delivery_core::{
    ClaimRequest, Claimed, ExpiredLease, Job, JobId, NewEvent, NewJob, Store, StoreError,
};

const OPEN_ACTION: &str = "open";

pub struct MakoNotifier {
    command: PathBuf,
    opener: PathBuf,
}

impl MakoNotifier {
    pub fn new(command: PathBuf, opener: PathBuf) -> Self {
        Self { command, opener }
    }
}

pub fn mako_args(n: &Notification) -> Vec<String> {
    vec![
        "--app-name=pizza-delivery".to_owned(),
        format!("--urgency={}", n.urgency.as_str()),
        format!("--action={OPEN_ACTION}=Open job"),
        "--wait".to_owned(),
        "--".to_owned(),
        n.title.clone(),
        n.body.clone(),
    ]
}

impl Notifier for MakoNotifier {
    fn notify(&self, n: &Notification) -> Result<(), NotifyError> {
        let mut child = Command::new(&self.command)
            .args(mako_args(n))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| NotifyError(format!("{}: {e}", self.command.display())))?;
        let (opener, url) = (self.opener.clone(), n.url.clone());
        std::thread::spawn(move || {
            let mut out = String::new();
            if let Some(mut stdout) = child.stdout.take() {
                let _ = stdout.read_to_string(&mut out);
            }
            let _ = child.wait();
            if out.trim() == OPEN_ACTION {
                let _ = Command::new(opener)
                    .arg(url)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        });
        Ok(())
    }
}

pub struct NotifyingStore {
    inner: Box<dyn Store>,
    dispatcher: Dispatcher,
}

impl NotifyingStore {
    pub fn new(inner: Box<dyn Store>, dispatcher: Dispatcher) -> Self {
        Self { inner, dispatcher }
    }

    fn announce(&mut self, event: &Event) {
        let pr_url = match event.payload {
            EventPayload::JobCompleted {} => self
                .inner
                .read_timeline(&event.job_id)
                .ok()
                .and_then(|timeline| pr_url_of(&timeline)),
            _ => None,
        };
        if let Err(e) = self.dispatcher.dispatch(event, pr_url.as_deref()) {
            eprintln!("notify: job {}: {e}", event.job_id);
        }
    }
}

impl Store for NotifyingStore {
    fn enqueue_job(&mut self, job: NewJob) -> Result<Job, StoreError> {
        self.inner.enqueue_job(job)
    }

    fn list_jobs(&self) -> Result<Vec<Job>, StoreError> {
        self.inner.list_jobs()
    }

    fn read_timeline(&self, job: &JobId) -> Result<Vec<Event>, StoreError> {
        self.inner.read_timeline(job)
    }

    fn read_timeline_cursored(&self, job: &JobId) -> Result<Vec<(u64, Event)>, StoreError> {
        self.inner.read_timeline_cursored(job)
    }

    fn head(&self) -> Result<u64, StoreError> {
        self.inner.head()
    }

    fn events_since(&self, after: u64, limit: usize) -> Result<Vec<(u64, Event)>, StoreError> {
        self.inner.events_since(after, limit)
    }

    fn quota_events(&self) -> Result<Vec<Event>, StoreError> {
        self.inner.quota_events()
    }

    fn append_event(&mut self, event: NewEvent) -> Result<Event, StoreError> {
        let stored = self.inner.append_event(event)?;
        self.announce(&stored);
        Ok(stored)
    }

    fn claim_stage(&mut self, request: ClaimRequest) -> Result<Option<Claimed>, StoreError> {
        self.inner.claim_stage(request)
    }

    fn heartbeat(&mut self, stage_id: &str, expires_ms: i64) -> Result<(), StoreError> {
        self.inner.heartbeat(stage_id, expires_ms)
    }

    fn set_lease_process(
        &mut self,
        stage_id: &str,
        pid: u32,
        start: u64,
    ) -> Result<(), StoreError> {
        self.inner.set_lease_process(stage_id, pid, start)
    }

    fn expired_leases(&self, now_ms: i64) -> Result<Vec<ExpiredLease>, StoreError> {
        self.inner.expired_leases(now_ms)
    }

    fn lease_of_job(&self, job: &JobId) -> Result<Option<ExpiredLease>, StoreError> {
        self.inner.lease_of_job(job)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};

    use pizza_delivery_core::event::Actor;
    use pizza_delivery_core::notify::{NotifyKind, Urgency};
    use pizza_delivery_core::Priority;

    use super::*;
    use crate::lock::acquire;
    use crate::sqlite_store::SqliteStore;

    struct Probe {
        seen: Seen,
        fail: bool,
    }

    impl Notifier for Probe {
        fn notify(&self, n: &Notification) -> Result<(), NotifyError> {
            if self.fail {
                return Err(NotifyError("mako is not running".into()));
            }
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(n.clone());
            }
            Ok(())
        }
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;
    type Seen = Arc<Mutex<Vec<Notification>>>;
    type Rig = (tempfile::TempDir, NotifyingStore, Seen);

    fn store_with(
        fail: bool,
        muted: HashSet<NotifyKind>,
    ) -> Result<Rig, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let inner = SqliteStore::open(&dir.path().join("l.db"))?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let dispatcher = Dispatcher::new(
            Box::new(Probe {
                seen: Arc::clone(&seen),
                fail,
            }),
            muted,
            "http://pd.local".into(),
        );
        Ok((dir, NotifyingStore::new(Box::new(inner), dispatcher), seen))
    }

    fn submit(store: &mut NotifyingStore, id: &str) -> Result<(), StoreError> {
        store
            .enqueue_job(NewJob {
                id: JobId::new(id),
                event_id: format!("ev-{id}"),
                prompt: "p".into(),
                repo: Some("/r".into()),
                priority: Priority::Normal,
                follow_up: None,
                submitted_at_ms: 1,
            })
            .map(|_| ())
    }

    fn append(
        store: &mut NotifyingStore,
        id: &str,
        n: u32,
        payload: EventPayload,
    ) -> Result<Event, StoreError> {
        store.append_event(NewEvent {
            id: format!("e-{id}-{n}"),
            job_id: JobId::new(id),
            stage_id: None,
            ts_ms: 10,
            actor: Actor::Daemon,
            payload,
        })
    }

    fn audit(pr: &str) -> EventPayload {
        EventPayload::SandboxAudit {
            profile_hash: "h".into(),
            mounts: vec![],
            egress_allowlist: vec![],
            blocked_requests: vec![],
            mcp_activations: vec![],
            diff_policy_result: None,
            pushed_ref: None,
            pr_url: Some(pr.into()),
        }
    }

    #[test]
    fn completing_a_job_notifies_with_the_pr_link_from_the_ledger() -> TestResult {
        let (_d, mut store, seen) = store_with(false, HashSet::new())?;
        submit(&mut store, "J1")?;
        append(&mut store, "J1", 1, audit("https://gh/pull/9"))?;
        append(&mut store, "J1", 2, EventPayload::JobCompleted {})?;
        let seen = acquire(&seen, "seen")?;
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].urgency, Urgency::Normal);
        assert!(seen[0].body.contains("https://gh/pull/9"));
        Ok(())
    }

    #[test]
    fn a_failing_notifier_never_fails_the_ledger_write() -> TestResult {
        let (_d, mut store, _) = store_with(true, HashSet::new())?;
        submit(&mut store, "J1")?;
        let stored = append(
            &mut store,
            "J1",
            1,
            EventPayload::JobFailed { reason: "x".into() },
        )?;
        assert_eq!(stored.payload.kind(), "job-failed");
        assert_eq!(store.read_timeline(&JobId::new("J1"))?.len(), 2);
        Ok(())
    }

    #[test]
    fn muted_types_through_the_store_are_not_delivered() -> TestResult {
        let muted = HashSet::from([NotifyKind::Failed]);
        let (_d, mut store, seen) = store_with(false, muted)?;
        submit(&mut store, "J1")?;
        append(
            &mut store,
            "J1",
            1,
            EventPayload::JobFailed { reason: "x".into() },
        )?;
        assert!(acquire(&seen, "seen")?.is_empty());
        Ok(())
    }

    #[test]
    fn mako_args_carry_urgency_title_body_and_an_open_action() {
        let n = Notification {
            job: JobId::new("J1"),
            kind: NotifyKind::Failed,
            urgency: Urgency::Critical,
            title: "pizza-delivery J1".into(),
            body: "Job failed: x".into(),
            url: "http://pd.local/jobs/J1".into(),
        };
        let args = mako_args(&n);
        assert!(args.contains(&"--urgency=critical".to_owned()));
        assert!(args.iter().any(|a| a.starts_with("--action=open=")));
        assert_eq!(
            &args[args.len() - 2..],
            ["pizza-delivery J1", "Job failed: x"]
        );
    }
}
