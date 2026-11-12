use thiserror::Error;

use crate::event::{Actor, Event, EventPayload};
use crate::id::JobId;
use crate::job::Job;
use crate::schedule::{Caps, Priority};
use crate::triage::TriageOptions;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    pub id: JobId,
    pub event_id: String,
    pub prompt: String,
    pub repo: Option<String>,
    pub priority: Priority,
    pub options: Option<TriageOptions>,
    pub submitted_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEvent {
    pub id: String,
    pub job_id: JobId,
    pub stage_id: Option<String>,
    pub ts_ms: i64,
    pub actor: Actor,
    pub payload: EventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRequest {
    pub holder: String,
    pub now_ms: i64,
    pub ttl_ms: i64,
    pub stage_id: String,
    pub event_ids: [String; 2],
    pub require_triage: bool,
    pub agent: String,
    pub caps: Caps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    pub job: Job,
    pub stage_id: String,
    pub n: u32,
    pub name: String,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredLease {
    pub job_id: JobId,
    pub stage_id: String,
    pub holder: String,
    pub pid: Option<u32>,
    pub pid_start: Option<u64>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store failure: {0}")]
    Backend(String),
}

pub trait Store: Send {
    fn enqueue_job(&mut self, job: NewJob) -> Result<Job, StoreError>;
    fn list_jobs(&self) -> Result<Vec<Job>, StoreError>;
    fn read_timeline(&self, job: &JobId) -> Result<Vec<Event>, StoreError>;
    fn quota_events(&self) -> Result<Vec<Event>, StoreError>;
    fn append_event(&mut self, event: NewEvent) -> Result<Event, StoreError>;
    fn claim_stage(&mut self, request: ClaimRequest) -> Result<Option<Claimed>, StoreError>;
    fn heartbeat(&mut self, stage_id: &str, expires_ms: i64) -> Result<(), StoreError>;
    fn set_lease_process(&mut self, stage_id: &str, pid: u32, start: u64)
        -> Result<(), StoreError>;
    fn expired_leases(&self, now_ms: i64) -> Result<Vec<ExpiredLease>, StoreError>;
    fn lease_of_job(&self, job: &JobId) -> Result<Option<ExpiredLease>, StoreError>;
}
