use pizza_delivery_core::api::{ApiError, ErrorKind, JobView};
use pizza_delivery_core::inbox::{fold_inbox, follow_up_prompt, EntryStatus, InboxEntry};
use pizza_delivery_core::{
    fold_quota, fold_stages, quota_views, resolve, short_id, Actor, EventPayload, FollowUp, Job,
    JobId, JobState, NewEvent, NewJob, Priority, Request, ResolveError, Response, Store,
    StoreError,
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

fn cancel(reference: &str, store: &mut dyn Store, env: &dyn Env) -> Response {
    let jobs = match store.list_jobs() {
        Ok(jobs) => jobs,
        Err(e) => return internal(e),
    };
    let all: Vec<JobId> = jobs.iter().map(|j| j.id.clone()).collect();
    let id = match resolve(reference, &all) {
        Ok(id) => id,
        Err(e) => return resolve_error(e),
    };
    let Some(job) = jobs.iter().find(|j| j.id == id) else {
        return error(ErrorKind::NotFound, "job vanished", vec![]);
    };
    if matches!(
        job.state,
        JobState::Completed | JobState::Failed | JobState::Cancelled
    ) {
        return error(
            ErrorKind::Usage,
            format!("job is already {}", job.state.as_str()),
            vec![],
        );
    }
    let event = NewEvent {
        id: env.new_ulid(),
        job_id: id.clone(),
        stage_id: None,
        ts_ms: env.now_ms(),
        actor: Actor::Human,
        payload: EventPayload::JobCancelled {},
    };
    match store.append_event(event) {
        Ok(_) => Response::Cancelled {
            short_id: short_id(&id, &all),
            id,
        },
        Err(e) => internal(e),
    }
}

#[allow(clippy::result_large_err)]
fn load_inbox(store: &dyn Store) -> Result<(Vec<Job>, Vec<InboxEntry>), Response> {
    let jobs = store.list_jobs().map_err(internal)?;
    let mut events = Vec::new();
    for job in &jobs {
        events.extend(store.read_timeline(&job.id).map_err(internal)?);
    }
    Ok((jobs, fold_inbox(&events)))
}

#[allow(clippy::result_large_err)]
fn resolve_entry<'a>(
    reference: &str,
    entries: &'a [InboxEntry],
) -> Result<&'a InboxEntry, Response> {
    let ids: Vec<JobId> = entries.iter().map(|e| JobId::new(e.id.clone())).collect();
    match resolve(reference, &ids) {
        Ok(id) => entries
            .iter()
            .find(|e| e.id == id.as_str())
            .ok_or_else(|| error(ErrorKind::NotFound, "entry vanished", vec![])),
        Err(ResolveError::NotFound(_)) => Err(error(
            ErrorKind::NotFound,
            format!("no feedback entry matches '{reference}'"),
            vec![],
        )),
        Err(e) => Err(resolve_error(e)),
    }
}

fn not_open(entry: &InboxEntry) -> Response {
    error(
        ErrorKind::Usage,
        format!(
            "entry {} is already {}",
            entry.short_id,
            entry.status.as_str()
        ),
        vec![],
    )
}

fn dismiss(references: &[String], store: &mut dyn Store, env: &dyn Env) -> Response {
    let (_, entries) = match load_inbox(store) {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let mut chosen: Vec<&InboxEntry> = Vec::new();
    for reference in references {
        match resolve_entry(reference, &entries) {
            Ok(entry) if entry.status == EntryStatus::Open => chosen.push(entry),
            Ok(entry) => return not_open(entry),
            Err(response) => return response,
        }
    }
    if chosen.is_empty() {
        return error(ErrorKind::Usage, "name at least one entry", vec![]);
    }
    for entry in &chosen {
        let event = NewEvent {
            id: env.new_ulid(),
            job_id: entry.job.clone(),
            stage_id: None,
            ts_ms: env.now_ms(),
            actor: Actor::Human,
            payload: EventPayload::FeedbackDismissed {
                entry: entry.id.clone(),
            },
        };
        if let Err(e) = store.append_event(event) {
            return internal(e);
        }
    }
    Response::Dismissed {
        entries: chosen.iter().map(|e| e.id.clone()).collect(),
    }
}

fn reply(
    targets: &[String],
    text: &str,
    priority: Option<Priority>,
    store: &mut dyn Store,
    env: &dyn Env,
) -> Response {
    let text = text.trim();
    if text.is_empty() {
        return error(ErrorKind::Usage, "reply text must not be empty", vec![]);
    }
    let (jobs, entries) = match load_inbox(store) {
        Ok(loaded) => loaded,
        Err(response) => return response,
    };
    let job_ids: Vec<JobId> = jobs.iter().map(|j| j.id.clone()).collect();
    let mut chosen: Vec<&InboxEntry> = Vec::new();
    for target in targets {
        match resolve(target, &job_ids) {
            Ok(job) => chosen.extend(
                entries
                    .iter()
                    .filter(|e| e.job == job && e.status == EntryStatus::Open),
            ),
            Err(ResolveError::NotFound(_)) => match resolve_entry(target, &entries) {
                Ok(entry) => chosen.push(entry),
                Err(response) => return response,
            },
            Err(e) => return resolve_error(e),
        }
    }
    chosen.sort_by(|a, b| a.id.cmp(&b.id));
    chosen.dedup_by(|a, b| a.id == b.id);
    let Some(first) = chosen.first() else {
        return error(
            ErrorKind::Usage,
            "no open feedback entries to answer",
            vec![],
        );
    };
    if let Some(done) = chosen.iter().find(|e| e.status != EntryStatus::Open) {
        return not_open(done);
    }
    if chosen.iter().any(|e| e.job != first.job) {
        return error(
            ErrorKind::Usage,
            "a reply answers entries of one job",
            vec![],
        );
    }
    let Some(parent) = jobs.iter().find(|j| j.id == first.job) else {
        return error(ErrorKind::NotFound, "job vanished", vec![]);
    };
    let answered: Vec<String> = chosen.iter().map(|e| e.id.clone()).collect();
    let new = NewJob {
        id: JobId::new(env.new_ulid()),
        event_id: env.new_ulid(),
        prompt: follow_up_prompt(&parent.prompt, &chosen, text),
        repo: parent.repo.clone(),
        priority: priority.unwrap_or(Priority::High),
        follow_up: Some(FollowUp {
            parent: parent.id.clone(),
            entries: answered.clone(),
            reply: text.to_owned(),
        }),
        submitted_at_ms: env.now_ms(),
    };
    let result = store.enqueue_job(new).and_then(|job| {
        let all: Vec<JobId> = store.list_jobs()?.into_iter().map(|j| j.id).collect();
        Ok((short_id(&job.id, &all), job))
    });
    match result {
        Ok((short, job)) => Response::Replied {
            id: job.id,
            short_id: short,
            answered,
        },
        Err(e) => internal(e),
    }
}

pub fn handle(request: Request, store: &mut dyn Store, env: &dyn Env) -> Response {
    match request {
        Request::Submit { prompt, .. } if prompt.trim().is_empty() => {
            error(ErrorKind::Usage, "prompt must not be empty", vec![])
        }
        Request::Submit {
            prompt,
            repo,
            priority,
        } => {
            let new = NewJob {
                id: JobId::new(env.new_ulid()),
                event_id: env.new_ulid(),
                prompt,
                repo,
                priority: priority.unwrap_or_default(),
                follow_up: None,
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
        Request::Cancel { job: reference } => cancel(&reference, store, env),
        Request::Inbox { all } => match load_inbox(store) {
            Ok((_, mut entries)) => {
                if !all {
                    entries.retain(|e| e.status != EntryStatus::Dismissed);
                }
                Response::Inbox { entries }
            }
            Err(response) => response,
        },
        Request::InboxShow { entry } => match load_inbox(store) {
            Ok((_, entries)) => match resolve_entry(&entry, &entries) {
                Ok(found) => Response::InboxEntry {
                    entry: found.clone(),
                },
                Err(response) => response,
            },
            Err(response) => response,
        },
        Request::Dismiss { entries } => dismiss(&entries, store, env),
        Request::Reply {
            targets,
            text,
            priority,
        } => reply(&targets, &text, priority, store, env),
        Request::Stop => Response::Stopped,
    }
}
