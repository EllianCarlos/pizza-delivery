use pizza_delivery_core::api::{ApiError, ErrorKind, JobView};
use pizza_delivery_core::{
    check_range, filter_artifacts, filter_events, filter_inbox, fold_quota, fold_stages,
    quota_views, resolve, short_id, Actor, EventPayload, Job, JobId, JobState, NewEvent, NewJob,
    Request, ResolveError, Response, Store, StoreError,
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

/// Resolve job references to full ids, rewriting them in place.
fn resolve_refs(
    refs: &mut Vec<String>,
    since: Option<i64>,
    until: Option<i64>,
    store: &dyn Store,
) -> Result<(Vec<JobId>, Vec<pizza_delivery_core::Event>), Box<Response>> {
    check_range(since, until).map_err(|m| Box::new(error(ErrorKind::Usage, m, vec![])))?;
    let jobs = store.list_jobs().map_err(|e| Box::new(internal(e)))?;
    let all: Vec<JobId> = jobs.iter().map(|j| j.id.clone()).collect();
    let mut resolved = Vec::with_capacity(refs.len());
    for r in refs.iter() {
        resolved.push(resolve(r, &all).map_err(|e| Box::new(resolve_error(e)))?);
    }
    *refs = resolved.iter().map(|id| id.as_str().to_string()).collect();
    let scan: Vec<&JobId> = if resolved.is_empty() {
        all.iter().collect()
    } else {
        resolved.iter().collect()
    };
    let mut events = Vec::new();
    for id in scan {
        events.extend(store.read_timeline(id).map_err(|e| Box::new(internal(e)))?);
    }
    Ok((all, events))
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
        Request::Events {
            mut filter,
            query,
            limit,
        } => match resolve_refs(&mut filter.jobs, filter.since_ms, filter.until_ms, store) {
            Ok((_, events)) => Response::EventItems {
                items: filter_events(&events, &filter, query.as_deref(), limit),
            },
            Err(response) => *response,
        },
        Request::Artifacts {
            mut filter,
            query,
            limit,
        } => match resolve_refs(&mut filter.jobs, filter.since_ms, filter.until_ms, store) {
            Ok((all, events)) => Response::ArtifactItems {
                items: filter_artifacts(&events, &filter, query.as_deref(), limit, &all),
            },
            Err(response) => *response,
        },
        Request::Inbox {
            mut filter,
            query,
            limit,
        } => match resolve_refs(&mut filter.jobs, filter.since_ms, filter.until_ms, store) {
            Ok((all, events)) => Response::InboxItems {
                items: filter_inbox(&events, &filter, query.as_deref(), limit, &all),
            },
            Err(response) => *response,
        },
        Request::Stop => Response::Stopped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite_store::SqliteStore;
    use pizza_delivery_core::{ArtifactFilter, EventFilter, InboxFilter, Priority};
    use std::sync::atomic::{AtomicI64, Ordering};

    struct FakeEnv(AtomicI64);

    impl Env for FakeEnv {
        fn now_ms(&self) -> i64 {
            self.0.fetch_add(1000, Ordering::SeqCst)
        }
        fn new_ulid(&self) -> String {
            format!("U{:05}", self.0.fetch_add(1, Ordering::SeqCst))
        }
    }

    type R = Result<(), Box<dyn std::error::Error>>;

    fn seed() -> Result<(tempfile::TempDir, SqliteStore, FakeEnv), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let mut store = SqliteStore::open(&dir.path().join("l.db"))?;
        let env = FakeEnv(AtomicI64::new(1000));
        for (id, at) in [
            ("01AAAAAAAAAAAAAAAAAAAAAAAA", 1),
            ("01AABBBBBBBBBBBBBBBBBBBBBB", 2),
        ] {
            store.enqueue_job(NewJob {
                id: JobId::new(id),
                event_id: format!("E-{id}"),
                prompt: format!("prompt {id}"),
                repo: None,
                priority: Priority::Normal,
                submitted_at_ms: at,
            })?;
        }
        let add = |store: &mut SqliteStore, n: i64, job: &str, payload: EventPayload| {
            store.append_event(NewEvent {
                id: format!("EV{n}"),
                job_id: JobId::new(job),
                stage_id: Some("s1".into()),
                ts_ms: n * 10,
                actor: Actor::Agent,
                payload,
            })
        };
        add(
            &mut store,
            1,
            "01AAAAAAAAAAAAAAAAAAAAAAAA",
            EventPayload::FeedbackRecorded {
                kind: "risk".into(),
                severity: "high".into(),
                text: "Leaky Cache".into(),
                confidence: Some(90),
            },
        )?;
        add(
            &mut store,
            2,
            "01AABBBBBBBBBBBBBBBBBBBBBB",
            EventPayload::FeedbackRecorded {
                kind: "risk".into(),
                severity: "low".into(),
                text: "minor".into(),
                confidence: Some(20),
            },
        )?;
        add(
            &mut store,
            3,
            "01AABBBBBBBBBBBBBBBBBBBBBB",
            EventPayload::ArtifactRecorded {
                kind: "diff".into(),
                path: "src/a.rs".into(),
                sha256: "x".into(),
            },
        )?;
        Ok((dir, store, env))
    }

    fn events(
        store: &mut SqliteStore,
        env: &FakeEnv,
        filter: EventFilter,
        q: Option<&str>,
        limit: Option<usize>,
    ) -> Response {
        handle(
            Request::Events {
                filter,
                query: q.map(Into::into),
                limit,
            },
            store,
            env,
        )
    }

    #[test]
    fn events_filter_by_job_ref_last_and_and_or() -> R {
        let (_d, mut s, env) = seed()?;
        let Response::EventItems { items } = events(
            &mut s,
            &env,
            EventFilter {
                jobs: vec!["@last".into()],
                ..Default::default()
            },
            None,
            None,
        ) else {
            return Err("bad response".into());
        };
        assert!(items.iter().all(|e| e.job_id.as_str().starts_with("01AAB")));
        assert_eq!(items.len(), 3);
        let f = EventFilter {
            jobs: vec!["01AAA".into(), "01AAB".into()],
            kinds: vec!["feedback-recorded".into()],
            ..Default::default()
        };
        let Response::EventItems { items } = events(&mut s, &env, f, None, Some(1)) else {
            return Err("bad".into());
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "EV2");
        Ok(())
    }

    #[test]
    fn job_ref_errors_and_inverted_range_map_to_existing_kinds() -> R {
        let (_d, mut s, env) = seed()?;
        let kind = |r: Response| match r {
            Response::Error { error } => Some(error.kind),
            _ => None,
        };
        let nf = events(
            &mut s,
            &env,
            EventFilter {
                jobs: vec!["ZZZ".into()],
                ..Default::default()
            },
            None,
            None,
        );
        assert_eq!(kind(nf), Some(ErrorKind::NotFound));
        let amb = events(
            &mut s,
            &env,
            EventFilter {
                jobs: vec!["01AA".into()],
                ..Default::default()
            },
            None,
            None,
        );
        assert_eq!(kind(amb), Some(ErrorKind::Ambiguous));
        let inv = events(
            &mut s,
            &env,
            EventFilter {
                since_ms: Some(9),
                until_ms: Some(1),
                ..Default::default()
            },
            None,
            None,
        );
        assert_eq!(kind(inv), Some(ErrorKind::Usage));
        Ok(())
    }

    #[test]
    fn search_inbox_and_artifacts_through_the_handler() -> R {
        let (_d, mut s, env) = seed()?;
        let Response::InboxItems { items } = handle(
            Request::Inbox {
                filter: InboxFilter {
                    min_confidence: Some(50),
                    ..Default::default()
                },
                query: Some("CACHE".into()),
                limit: None,
            },
            &mut s,
            &env,
        ) else {
            return Err("bad".into());
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].severity, "high");
        let Response::InboxItems { items } = handle(
            Request::Inbox {
                filter: InboxFilter::default(),
                query: Some("nomatch".into()),
                limit: None,
            },
            &mut s,
            &env,
        ) else {
            return Err("bad".into());
        };
        assert!(items.is_empty());
        let Response::ArtifactItems { items } = handle(
            Request::Artifacts {
                filter: ArtifactFilter {
                    paths: vec!["src/".into()],
                    ..Default::default()
                },
                query: None,
                limit: None,
            },
            &mut s,
            &env,
        ) else {
            return Err("bad".into());
        };
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, "src/a.rs");
        Ok(())
    }
}
