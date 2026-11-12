use std::collections::HashSet;

use thiserror::Error;

use crate::event::{Event, EventPayload};
use crate::id::JobId;

pub const QUOTA_NOTIFY_THRESHOLD_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Urgency {
    Low,
    Normal,
    Critical,
}

impl Urgency {
    pub fn as_str(self) -> &'static str {
        match self {
            Urgency::Low => "low",
            Urgency::Normal => "normal",
            Urgency::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyKind {
    Completed,
    HighFeedback,
    Failed,
    NeedsHuman,
    QuotaParked,
}

impl NotifyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NotifyKind::Completed => "completed",
            NotifyKind::HighFeedback => "high-feedback",
            NotifyKind::Failed => "failed",
            NotifyKind::NeedsHuman => "needs-human",
            NotifyKind::QuotaParked => "quota-parked",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        [
            NotifyKind::Completed,
            NotifyKind::HighFeedback,
            NotifyKind::Failed,
            NotifyKind::NeedsHuman,
            NotifyKind::QuotaParked,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == text.trim())
    }

    pub fn urgency(self) -> Urgency {
        match self {
            NotifyKind::Completed | NotifyKind::HighFeedback => Urgency::Normal,
            NotifyKind::Failed | NotifyKind::NeedsHuman => Urgency::Critical,
            NotifyKind::QuotaParked => Urgency::Low,
        }
    }
}

pub fn parse_muted(list: &str) -> HashSet<NotifyKind> {
    list.split(',').filter_map(NotifyKind::parse).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub job: JobId,
    pub kind: NotifyKind,
    pub urgency: Urgency,
    pub title: String,
    pub body: String,
    pub url: String,
}

#[derive(Debug, Error)]
#[error("notification failed: {0}")]
pub struct NotifyError(pub String);

pub trait Notifier: Send {
    fn notify(&self, notification: &Notification) -> Result<(), NotifyError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    Muted,
    Coalesced,
    Ignored,
}

pub fn pr_url_of(timeline: &[Event]) -> Option<String> {
    timeline.iter().rev().find_map(|e| match &e.payload {
        EventPayload::SandboxAudit {
            pr_url: Some(url), ..
        } => Some(url.clone()),
        _ => None,
    })
}

fn classify(event: &Event) -> Option<(NotifyKind, String)> {
    match &event.payload {
        EventPayload::JobCompleted {} => Some((NotifyKind::Completed, "Job completed".to_owned())),
        EventPayload::JobFailed { reason } => {
            Some((NotifyKind::Failed, format!("Job failed: {reason}")))
        }
        EventPayload::JobNeedsHuman { reason } => {
            Some((NotifyKind::NeedsHuman, format!("Needs you: {reason}")))
        }
        EventPayload::FeedbackRecorded { severity, text, .. } if severity == "high" => {
            Some((NotifyKind::HighFeedback, format!("High severity: {text}")))
        }
        EventPayload::JobParked { until_ms, reason }
            if until_ms.saturating_sub(event.ts_ms) > QUOTA_NOTIFY_THRESHOLD_MS =>
        {
            Some((
                NotifyKind::QuotaParked,
                format!("Waiting for quota: {reason}"),
            ))
        }
        _ => None,
    }
}

pub fn notification_for(
    event: &Event,
    pr_url: Option<&str>,
    base_url: &str,
) -> Option<Notification> {
    let (kind, summary) = classify(event)?;
    let body = match (kind, pr_url) {
        (NotifyKind::Completed, Some(url)) => format!("{summary}\n{url}"),
        _ => summary,
    };
    Some(Notification {
        job: event.job_id.clone(),
        kind,
        urgency: kind.urgency(),
        title: format!("pizza-delivery {}", event.job_id),
        body,
        url: format!("{}/jobs/{}", base_url.trim_end_matches('/'), event.job_id),
    })
}

pub struct Dispatcher {
    notifier: Box<dyn Notifier>,
    muted: HashSet<NotifyKind>,
    sent: HashSet<(JobId, NotifyKind)>,
    base_url: String,
}

impl Dispatcher {
    pub fn new(notifier: Box<dyn Notifier>, muted: HashSet<NotifyKind>, base_url: String) -> Self {
        Self {
            notifier,
            muted,
            sent: HashSet::new(),
            base_url,
        }
    }

    pub fn dispatch(
        &mut self,
        event: &Event,
        pr_url: Option<&str>,
    ) -> Result<Delivery, NotifyError> {
        let Some(notification) = notification_for(event, pr_url, &self.base_url) else {
            return Ok(Delivery::Ignored);
        };
        if self.muted.contains(&notification.kind) {
            return Ok(Delivery::Muted);
        }
        let key = (notification.job.clone(), notification.kind);
        if self.sent.contains(&key) {
            return Ok(Delivery::Coalesced);
        }
        self.notifier.notify(&notification)?;
        self.sent.insert(key);
        Ok(Delivery::Sent)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::event::Actor;

    type Seen = Arc<Mutex<Vec<Notification>>>;

    struct Recorder {
        seen: Seen,
        fail: bool,
    }

    impl Notifier for Recorder {
        fn notify(&self, n: &Notification) -> Result<(), NotifyError> {
            if self.fail {
                return Err(NotifyError("no daemon".into()));
            }
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(n.clone());
            }
            Ok(())
        }
    }

    fn event(job: &str, ts_ms: i64, payload: EventPayload) -> Event {
        Event {
            id: "e".into(),
            job_id: JobId::new(job),
            stage_id: None,
            seq: 1,
            ts_ms,
            actor: Actor::Daemon,
            payload,
        }
    }

    fn feedback(severity: &str) -> EventPayload {
        EventPayload::FeedbackRecorded {
            kind: "risk".into(),
            severity: severity.into(),
            text: "t".into(),
            confidence: None,
            alternatives: vec![],
            impact: None,
        }
    }

    fn failed() -> EventPayload {
        EventPayload::JobFailed { reason: "x".into() }
    }

    fn rig(muted: &[NotifyKind], fail: bool) -> (Dispatcher, Seen) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let notifier = Recorder {
            seen: Arc::clone(&seen),
            fail,
        };
        let dispatcher = Dispatcher::new(
            Box::new(notifier),
            muted.iter().copied().collect(),
            "http://pd.local".into(),
        );
        (dispatcher, seen)
    }

    fn urgencies(seen: &Seen) -> Vec<Urgency> {
        seen.lock()
            .map(|s| s.iter().map(|n| n.urgency).collect())
            .unwrap_or_default()
    }

    #[test]
    fn urgency_follows_the_spec_for_each_event() {
        let (mut d, seen) = rig(&[], false);
        let events = [
            event("J1", 0, EventPayload::JobCompleted {}),
            event("J2", 0, feedback("high")),
            event("J3", 0, failed()),
            event("J4", 0, EventPayload::JobNeedsHuman { reason: "x".into() }),
            event(
                "J5",
                0,
                EventPayload::JobParked {
                    until_ms: 16 * 60_000,
                    reason: "quota".into(),
                },
            ),
        ];
        for e in &events {
            assert!(matches!(d.dispatch(e, None), Ok(Delivery::Sent)));
        }
        assert_eq!(
            urgencies(&seen),
            vec![
                Urgency::Normal,
                Urgency::Normal,
                Urgency::Critical,
                Urgency::Critical,
                Urgency::Low
            ]
        );
    }

    #[test]
    fn short_parks_low_feedback_and_progress_events_stay_silent() {
        let (mut d, seen) = rig(&[], false);
        let quiet = [
            event(
                "J1",
                0,
                EventPayload::JobParked {
                    until_ms: 15 * 60_000,
                    reason: "quota".into(),
                },
            ),
            event("J1", 0, feedback("low")),
            event("J1", 0, EventPayload::StageSucceeded {}),
        ];
        for e in &quiet {
            assert!(matches!(d.dispatch(e, None), Ok(Delivery::Ignored)));
        }
        assert!(urgencies(&seen).is_empty());
    }

    #[test]
    fn a_completed_notification_carries_the_pr_link_and_opens_the_job_page() {
        let n = notification_for(
            &event("J1", 0, EventPayload::JobCompleted {}),
            Some("https://github.com/o/r/pull/3"),
            "http://pd.local/",
        );
        assert_eq!(
            n.as_ref().map(|n| n.url.as_str()),
            Some("http://pd.local/jobs/J1")
        );
        assert!(n
            .map(|n| n.body.contains("https://github.com/o/r/pull/3"))
            .unwrap_or(false));
    }

    #[test]
    fn pr_link_is_read_from_the_sandbox_audit() {
        let audit = EventPayload::SandboxAudit {
            profile_hash: "h".into(),
            mounts: vec![],
            egress_allowlist: vec![],
            blocked_requests: vec![],
            mcp_activations: vec![],
            diff_policy_result: None,
            pushed_ref: None,
            pr_url: Some("https://x/pull/1".into()),
        };
        assert_eq!(
            pr_url_of(&[event("J1", 0, audit)]),
            Some("https://x/pull/1".into())
        );
        assert_eq!(pr_url_of(&[]), None);
    }

    #[test]
    fn repeated_states_coalesce_to_one_notification_per_job() {
        let (mut d, seen) = rig(&[], false);
        let high = event("J1", 0, feedback("high"));
        assert!(matches!(d.dispatch(&high, None), Ok(Delivery::Sent)));
        assert!(matches!(d.dispatch(&high, None), Ok(Delivery::Coalesced)));
        let other_job = event("J2", 0, feedback("high"));
        assert!(matches!(d.dispatch(&other_job, None), Ok(Delivery::Sent)));
        let other_state = event("J1", 0, failed());
        assert!(matches!(d.dispatch(&other_state, None), Ok(Delivery::Sent)));
        assert_eq!(urgencies(&seen).len(), 3);
    }

    #[test]
    fn a_muted_event_type_is_not_delivered_but_others_are() {
        let (mut d, seen) = rig(&[NotifyKind::QuotaParked, NotifyKind::Completed], false);
        let parked = event(
            "J1",
            0,
            EventPayload::JobParked {
                until_ms: 3_600_000,
                reason: "q".into(),
            },
        );
        assert!(matches!(d.dispatch(&parked, None), Ok(Delivery::Muted)));
        let done = event("J1", 0, EventPayload::JobCompleted {});
        assert!(matches!(d.dispatch(&done, None), Ok(Delivery::Muted)));
        assert!(matches!(
            d.dispatch(&event("J1", 0, failed()), None),
            Ok(Delivery::Sent)
        ));
        assert_eq!(urgencies(&seen), vec![Urgency::Critical]);
    }

    #[test]
    fn a_failed_delivery_is_reported_and_may_be_retried_later() {
        let (mut d, _) = rig(&[], true);
        let done = event("J1", 0, EventPayload::JobCompleted {});
        assert!(d.dispatch(&done, None).is_err());
        assert!(d.dispatch(&done, None).is_err());
    }

    #[test]
    fn muted_list_parses_known_names_and_skips_unknown_ones() {
        let muted = parse_muted("quota-parked, completed,bogus");
        assert_eq!(muted.len(), 2);
        assert!(muted.contains(&NotifyKind::Completed));
    }
}
