use thiserror::Error;

use crate::event::Event;
use crate::id::JobId;
use crate::job::Job;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewJob {
    pub id: JobId,
    pub event_id: String,
    pub prompt: String,
    pub submitted_at_ms: i64,
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
}
