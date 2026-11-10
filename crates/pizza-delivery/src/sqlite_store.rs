use std::path::Path;

use pizza_delivery_core::event::{Actor, Event, EventPayload};
use pizza_delivery_core::job::{apply, JobState};
use pizza_delivery_core::{Job, JobId, NewJob, Store, StoreError};
use rusqlite::{params, Connection, Transaction};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS events (
    global_seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    job_id     TEXT NOT NULL,
    stage_id   TEXT,
    seq        INTEGER NOT NULL,
    ts_ms      INTEGER NOT NULL,
    actor      TEXT NOT NULL,
    kind       TEXT NOT NULL,
    payload    TEXT NOT NULL,
    UNIQUE (job_id, seq)
);
CREATE TRIGGER IF NOT EXISTS events_no_update BEFORE UPDATE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only: update rejected'); END;
CREATE TRIGGER IF NOT EXISTS events_no_delete BEFORE DELETE ON events
BEGIN SELECT RAISE(ABORT, 'events are append-only: delete rejected'); END;
CREATE TABLE IF NOT EXISTS jobs (
    id              TEXT PRIMARY KEY,
    state           TEXT NOT NULL,
    prompt          TEXT NOT NULL,
    submitted_at_ms INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL
);
";

pub struct SqliteStore {
    conn: Connection,
}

fn backend(e: impl std::fmt::Display) -> StoreError {
    StoreError::Backend(e.to_string())
}

impl SqliteStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(backend)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(backend)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(backend)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(backend)?;
        conn.execute_batch(SCHEMA).map_err(backend)?;
        Ok(Self { conn })
    }
}

fn append(tx: &Transaction<'_>, event: &Event) -> Result<(), StoreError> {
    let payload = serde_json::to_value(&event.payload).map_err(backend)?;
    let body = payload
        .get("payload")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    tx.execute(
        "INSERT INTO events (id, job_id, stage_id, seq, ts_ms, actor, kind, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            event.id,
            event.job_id.as_str(),
            event.stage_id,
            event.seq as i64,
            event.ts_ms,
            event.actor.as_str(),
            event.payload.kind(),
            body.to_string()
        ],
    )
    .map_err(backend)?;
    Ok(())
}

fn project(tx: &Transaction<'_>, job: &Job) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO jobs (id, state, prompt, submitted_at_ms, last_seq) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, last_seq = excluded.last_seq",
        params![
            job.id.as_str(),
            job.state.as_str(),
            job.prompt,
            job.submitted_at_ms,
            job.last_seq as i64
        ],
    )
    .map_err(backend)?;
    Ok(())
}

fn parse_state(raw: &str) -> Result<JobState, StoreError> {
    match raw {
        "queued" => Ok(JobState::Queued),
        other => Err(backend(format!("unknown job state '{other}'"))),
    }
}

fn parse_actor(raw: &str) -> Result<Actor, StoreError> {
    match raw {
        "daemon" => Ok(Actor::Daemon),
        "agent" => Ok(Actor::Agent),
        "human" => Ok(Actor::Human),
        other => Err(backend(format!("unknown actor '{other}'"))),
    }
}

impl Store for SqliteStore {
    fn enqueue_job(&mut self, new: NewJob) -> Result<Job, StoreError> {
        let event = Event {
            id: new.event_id,
            job_id: new.id,
            stage_id: None,
            seq: 1,
            ts_ms: new.submitted_at_ms,
            actor: Actor::Human,
            payload: EventPayload::JobSubmitted { prompt: new.prompt },
        };
        let job = apply(None, &event).ok_or_else(|| backend("submission produced no job"))?;
        let tx = self.conn.transaction().map_err(backend)?;
        append(&tx, &event)?;
        project(&tx, &job)?;
        tx.commit().map_err(backend)?;
        Ok(job)
    }

    fn list_jobs(&self) -> Result<Vec<Job>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, state, prompt, submitted_at_ms, last_seq FROM jobs ORDER BY id")
            .map_err(backend)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })
            .map_err(backend)?;
        let mut jobs = Vec::new();
        for row in rows {
            let (id, state, prompt, submitted_at_ms, last_seq) = row.map_err(backend)?;
            jobs.push(Job {
                id: JobId::new(id),
                state: parse_state(&state)?,
                prompt,
                submitted_at_ms,
                last_seq: last_seq as u64,
            });
        }
        Ok(jobs)
    }

    fn read_timeline(&self, job: &JobId) -> Result<Vec<Event>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, stage_id, seq, ts_ms, actor, kind, payload
                 FROM events WHERE job_id = ?1 ORDER BY seq",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![job.as_str()], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(backend)?;
        let mut events = Vec::new();
        for row in rows {
            let (id, stage_id, seq, ts_ms, actor, kind, payload) = row.map_err(backend)?;
            let body: serde_json::Value = serde_json::from_str(&payload).map_err(backend)?;
            let payload: EventPayload =
                serde_json::from_value(serde_json::json!({"kind": kind, "payload": body}))
                    .map_err(backend)?;
            events.push(Event {
                id,
                job_id: job.clone(),
                stage_id,
                seq: seq as u64,
                ts_ms,
                actor: parse_actor(&actor)?,
                payload,
            });
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Result<(tempfile::TempDir, SqliteStore), Box<dyn std::error::Error>> {
        let dir = tempfile::TempDir::new()?;
        let s = SqliteStore::open(&dir.path().join("l.db"))?;
        Ok((dir, s))
    }

    fn new_job(id: &str) -> NewJob {
        NewJob {
            id: JobId::new(id),
            event_id: format!("E-{id}"),
            prompt: "p".into(),
            submitted_at_ms: 7,
        }
    }

    #[test]
    fn enqueue_then_list_and_timeline() -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1"))?;
        s.enqueue_job(new_job("J2"))?;
        let ids: Vec<_> = s.list_jobs()?.into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![JobId::new("J1"), JobId::new("J2")]);
        let tl = s.read_timeline(&JobId::new("J1"))?;
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].payload.kind(), "job-submitted");
        Ok(())
    }
}
