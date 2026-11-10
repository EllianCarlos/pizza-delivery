use pizza_delivery_core::api::{ApiError, ErrorKind, JobView};
use pizza_delivery_core::{
    fold_quota, fold_stages, quota_views, resolve, short_id, Job, JobId, NewJob, Request,
    ResolveError, Response, Store, StoreError,
};

pub trait Env: Send + Sync {
    fn now_ms(&self) -> i64;
    fn new_ulid(&self) -> String;
}

pub struct SystemEnv;

impl Env for SystemEnv {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default()
    }

    fn new_ulid(&self) -> String {
        ulid::Ulid::new().to_string()
    }
}

fn error(kind: ErrorKind, message: impl Into<String>, matches: Vec<JobId>) -> Response {
    Response::Error {
        error: ApiError {
            kind,
            message: message.into(),
            matches,
        },
    }
}

fn internal(e: StoreError) -> Response {
    error(ErrorKind::Internal, e.to_string(), vec![])
}

fn resolve_error(e: ResolveError) -> Response {
    match e {
        ResolveError::NotFound(_) => error(ErrorKind::NotFound, e.to_string(), vec![]),
        ResolveError::Ambiguous { ref matches, .. } => {
            error(ErrorKind::Ambiguous, e.to_string(), matches.clone())
        }
    }
}

fn view(job: &Job, all: &[JobId]) -> JobView {
    JobView {
        id: job.id.clone(),
        short_id: short_id(&job.id, all),
        state: job.state,
        prompt: job.prompt.clone(),
        submitted_at_ms: job.submitted_at_ms,
    }
}

pub fn handle(request: Request, store: &mut dyn Store, env: &dyn Env) -> Response {
    match request {
        Request::Submit { prompt, .. } if prompt.trim().is_empty() => {
            error(ErrorKind::Usage, "prompt must not be empty", vec![])
        }
        Request::Submit { prompt, repo } => {
            let new = NewJob {
                id: JobId::new(env.new_ulid()),
                event_id: env.new_ulid(),
                prompt,
                repo,
                submitted_at_ms: env.now_ms(),
            };
            let result = store.enqueue_job(new).and_then(|job| {
                let all: Vec<JobId> = store.list_jobs()?.into_iter().map(|j| j.id).collect();
                Ok((short_id(&job.id, &all), job))
            });
            match result {
                Ok((short, job)) => Response::Submitted {
                    id: job.id,
                    short_id: short,
                },
                Err(e) => internal(e),
            }
        }
        Request::Status { job: None } => match store.list_jobs() {
            Ok(jobs) => {
                let all: Vec<JobId> = jobs.iter().map(|j| j.id.clone()).collect();
                let mut views: Vec<JobView> = jobs.iter().map(|j| view(j, &all)).collect();
                views.sort_by(|a, b| b.id.cmp(&a.id));
                Response::Jobs { jobs: views }
            }
            Err(e) => internal(e),
        },
        Request::Status {
            job: Some(reference),
        } => {
            let jobs = match store.list_jobs() {
                Ok(jobs) => jobs,
                Err(e) => return internal(e),
            };
            let all: Vec<JobId> = jobs.iter().map(|j| j.id.clone()).collect();
            let id = match resolve(&reference, &all) {
                Ok(id) => id,
                Err(e) => return resolve_error(e),
            };
            let found = jobs.iter().find(|j| j.id == id);
            match (found, store.read_timeline(&id)) {
                (Some(job), Ok(events)) => Response::Job {
                    job: view(job, &all),
                    stages: fold_stages(&events),
                    events,
                },
                (None, _) => error(ErrorKind::NotFound, "job vanished", vec![]),
                (_, Err(e)) => internal(e),
            }
        }
        Request::Quota => match store.quota_events() {
            Ok(events) => {
                let now_ms = env.now_ms();
                Response::Quota {
                    agents: quota_views(&fold_quota(&events), now_ms),
                    now_ms,
                }
            }
            Err(e) => internal(e),
        },
        Request::Triage { .. } => error(
            ErrorKind::Internal,
            "triage requests are served by the daemon",
            vec![],
        ),
        Request::Stop => Response::Stopped,
    }
}
