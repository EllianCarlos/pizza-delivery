use std::path::Path;

use pizza_delivery_core::event::{Actor, Event, EventPayload};
use pizza_delivery_core::job::{apply, JobState};
use pizza_delivery_core::{
    next_claim, pick, Candidate, ClaimPlan, ClaimRequest, Claimed, ExpiredLease, Job, JobId,
    NewEvent, NewJob, Priority, Slot, Store, StoreError,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};

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
    repo            TEXT,
    priority        INTEGER NOT NULL DEFAULT 1,
    submitted_at_ms INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS leases (
    stage_id   TEXT PRIMARY KEY,
    job_id     TEXT NOT NULL,
    holder     TEXT NOT NULL,
    expires_ms INTEGER NOT NULL,
    pid        INTEGER,
    pid_start  INTEGER,
    agent      TEXT
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
        for migration in [
            "ALTER TABLE jobs ADD COLUMN priority INTEGER NOT NULL DEFAULT 1",
            "ALTER TABLE leases ADD COLUMN agent TEXT",
        ] {
            let _ = conn.execute(migration, []);
        }
        Ok(Self { conn })
    }
}

fn event_row(tx: &Transaction<'_>, event: &Event) -> Result<(), StoreError> {
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
        "INSERT INTO jobs (id, state, prompt, repo, priority, submitted_at_ms, last_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, repo = excluded.repo,
             priority = excluded.priority, last_seq = excluded.last_seq",
        params![
            job.id.as_str(),
            job.state.as_str(),
            job.prompt,
            job.repo,
            job.priority.rank(),
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
        "running" => Ok(JobState::Running),
        "waiting-for-quota" => Ok(JobState::WaitingForQuota),
        "completed" => Ok(JobState::Completed),
        "failed" => Ok(JobState::Failed),
        "needs-human" => Ok(JobState::NeedsHuman),
        "cancelled" => Ok(JobState::Cancelled),
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

type JobRow = (String, String, String, Option<String>, i64, i64, i64);

const JOB_COLUMNS: &str = "id, state, prompt, repo, submitted_at_ms, last_seq, priority";

fn map_job_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<JobRow> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
    ))
}

fn job_from_row(row: JobRow) -> Result<Job, StoreError> {
    let (id, state, prompt, repo, submitted_at_ms, last_seq, priority) = row;
    Ok(Job {
        id: JobId::new(id),
        state: parse_state(&state)?,
        prompt,
        repo,
        priority: Priority::from_rank(priority),
        submitted_at_ms,
        last_seq: last_seq as u64,
    })
}

fn load_job(tx: &Transaction<'_>, id: &JobId) -> Result<Option<Job>, StoreError> {
    let row = tx
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id = ?1"),
            params![id.as_str()],
            map_job_row,
        )
        .optional()
        .map_err(backend)?;
    row.map(job_from_row).transpose()
}

fn events_where(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<Event>, StoreError> {
    Ok(cursored_where(conn, filter, args)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

fn cursored_where(
    conn: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<(u64, Event)>, StoreError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, job_id, stage_id, seq, ts_ms, actor, kind, payload, global_seq
             FROM events WHERE {filter} ORDER BY global_seq"
        ))
        .map_err(backend)?;
    let rows = stmt
        .query_map(args, |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
                r.get::<_, i64>(8)?,
            ))
        })
        .map_err(backend)?;
    let mut events = Vec::new();
    for row in rows {
        let (id, job_id, stage_id, seq, ts_ms, actor, kind, payload, cursor) =
            row.map_err(backend)?;
        let body: serde_json::Value = serde_json::from_str(&payload).map_err(backend)?;
        let payload: EventPayload =
            serde_json::from_value(serde_json::json!({"kind": kind, "payload": body}))
                .map_err(backend)?;
        events.push((
            cursor as u64,
            Event {
                id,
                job_id: JobId::new(job_id),
                stage_id,
                seq: seq as u64,
                ts_ms,
                actor: parse_actor(&actor)?,
                payload,
            },
        ));
    }
    Ok(events)
}

fn timeline_in(conn: &Connection, job: &JobId) -> Result<Vec<Event>, StoreError> {
    events_where(conn, "job_id = ?1", &[&job.as_str()])
}

fn lease_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExpiredLease> {
    Ok(ExpiredLease {
        job_id: JobId::new(r.get::<_, String>(0)?),
        stage_id: r.get(1)?,
        holder: r.get(2)?,
        pid: r.get::<_, Option<i64>>(3)?.map(|p| p as u32),
        pid_start: r.get::<_, Option<i64>>(4)?.map(|p| p as u64),
    })
}

fn running_slots(conn: &Connection) -> Result<Vec<Slot>, StoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT COALESCE(j.repo, ''), COALESCE(l.agent, '') FROM leases l
             JOIN jobs j ON j.id = l.job_id",
        )
        .map_err(backend)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Slot {
                repo: r.get(0)?,
                agent: r.get(1)?,
            })
        })
        .map_err(backend)?;
    rows.map(|row| row.map_err(backend)).collect()
}

fn lease_effects(tx: &Transaction<'_>, event: &Event) -> Result<(), StoreError> {
    let Some(stage_id) = event.stage_id.as_deref() else {
        return Ok(());
    };
    match &event.payload {
        EventPayload::StageClaimed {
            holder,
            lease_expires_ms,
            ..
        } => tx
            .execute(
                "INSERT OR REPLACE INTO leases (stage_id, job_id, holder, expires_ms, pid, pid_start)
                 VALUES (?1, ?2, ?3, ?4, NULL, NULL)",
                params![stage_id, event.job_id.as_str(), holder, lease_expires_ms],
            )
            .map(|_| ())
            .map_err(backend),
        EventPayload::LeaseExpired { .. } | EventPayload::LeaseReleased { .. } => tx
            .execute("DELETE FROM leases WHERE stage_id = ?1", params![stage_id])
            .map(|_| ())
            .map_err(backend),
        _ => Ok(()),
    }
}

fn append_in(tx: &Transaction<'_>, new: NewEvent) -> Result<(Event, Job), StoreError> {
    let seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM events WHERE job_id = ?1",
            params![new.job_id.as_str()],
            |r| r.get(0),
        )
        .map_err(backend)?;
    let event = Event {
        id: new.id,
        job_id: new.job_id,
        stage_id: new.stage_id,
        seq: seq as u64,
        ts_ms: new.ts_ms,
        actor: new.actor,
        payload: new.payload,
    };
    let current = load_job(tx, &event.job_id)?;
    let job = apply(current, &event).ok_or_else(|| backend("event for unknown job"))?;
    event_row(tx, &event)?;
    project(tx, &job)?;
    lease_effects(tx, &event)?;
    Ok((event, job))
}

impl Store for SqliteStore {
    fn enqueue_job(&mut self, new: NewJob) -> Result<Job, StoreError> {
        let event = NewEvent {
            id: new.event_id,
            job_id: new.id,
            stage_id: None,
            ts_ms: new.submitted_at_ms,
            actor: Actor::Human,
            payload: EventPayload::JobSubmitted {
                prompt: new.prompt,
                repo: new.repo,
                priority: new.priority,
                follow_up: new.follow_up,
            },
        };
        let tx = self.conn.transaction().map_err(backend)?;
        let (_, job) = append_in(&tx, event)?;
        tx.commit().map_err(backend)?;
        Ok(job)
    }

    fn list_jobs(&self) -> Result<Vec<Job>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {JOB_COLUMNS} FROM jobs ORDER BY id"))
            .map_err(backend)?;
        let rows = stmt.query_map([], map_job_row).map_err(backend)?;
        rows.map(|row| job_from_row(row.map_err(backend)?))
            .collect()
    }

    fn read_timeline(&self, job: &JobId) -> Result<Vec<Event>, StoreError> {
        timeline_in(&self.conn, job)
    }

    fn read_timeline_cursored(&self, job: &JobId) -> Result<Vec<(u64, Event)>, StoreError> {
        cursored_where(&self.conn, "job_id = ?1", &[&job.as_str()])
    }

    fn head(&self) -> Result<u64, StoreError> {
        self.conn
            .query_row("SELECT COALESCE(MAX(global_seq), 0) FROM events", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
            .map_err(backend)
    }

    fn events_since(&self, after: u64, limit: usize) -> Result<Vec<(u64, Event)>, StoreError> {
        let after = after as i64;
        let limit = limit as i64;
        cursored_where(
            &self.conn,
            "global_seq > ?1 AND global_seq IN (SELECT global_seq FROM events WHERE global_seq > ?1 ORDER BY global_seq LIMIT ?2)",
            &[&after, &limit],
        )
    }

    fn quota_events(&self) -> Result<Vec<Event>, StoreError> {
        events_where(
            &self.conn,
            "global_seq IN (SELECT MAX(global_seq) FROM events WHERE kind = 'quota-signal'
                            GROUP BY json_extract(payload, '$.agent'))",
            &[],
        )
    }

    fn append_event(&mut self, event: NewEvent) -> Result<Event, StoreError> {
        let tx = self.conn.transaction().map_err(backend)?;
        let (event, _) = append_in(&tx, event)?;
        tx.commit().map_err(backend)?;
        Ok(event)
    }

    fn claim_stage(&mut self, request: ClaimRequest) -> Result<Option<Claimed>, StoreError> {
        let mut plans: Vec<(Job, ClaimPlan)> = Vec::new();
        for job in self.list_jobs()?.into_iter().filter(|j| {
            j.repo.is_some() && matches!(j.state, JobState::Queued | JobState::WaitingForQuota)
        }) {
            let events = timeline_in(&self.conn, &job.id)?;
            if request.require_triage
                && !events
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::TriageCompleted { .. }))
            {
                continue;
            }
            if let Some(plan) = next_claim(&events, request.now_ms) {
                plans.push((job, plan));
            }
        }
        let candidates: Vec<Candidate> = plans
            .iter()
            .map(|(job, _)| Candidate {
                job_id: job.id.clone(),
                priority: job.priority,
                submitted_at_ms: job.submitted_at_ms,
                repo: job.repo.clone().unwrap_or_default(),
            })
            .collect();
        let running = running_slots(&self.conn)?;
        let Some(chosen) = pick(&candidates, &running, &request.agent, request.caps) else {
            return Ok(None);
        };
        let Some((job, plan)) = plans.into_iter().nth(chosen) else {
            return Ok(None);
        };
        let [planned_id, claimed_id] = request.event_ids.clone();
        let stage_id = plan
            .existing_stage_id
            .clone()
            .unwrap_or_else(|| request.stage_id.clone());
        let tx = self.conn.transaction().map_err(backend)?;
        if plan.existing_stage_id.is_none() {
            append_in(
                &tx,
                NewEvent {
                    id: planned_id,
                    job_id: job.id.clone(),
                    stage_id: Some(stage_id.clone()),
                    ts_ms: request.now_ms,
                    actor: Actor::Daemon,
                    payload: EventPayload::StagePlanned {
                        n: plan.n,
                        name: plan.name.clone(),
                    },
                },
            )?;
        }
        let (_, job) = append_in(
            &tx,
            NewEvent {
                id: claimed_id,
                job_id: job.id.clone(),
                stage_id: Some(stage_id.clone()),
                ts_ms: request.now_ms,
                actor: Actor::Daemon,
                payload: EventPayload::StageClaimed {
                    holder: request.holder.clone(),
                    attempt: plan.attempt,
                    lease_expires_ms: request.now_ms + request.ttl_ms,
                },
            },
        )?;
        tx.execute(
            "UPDATE leases SET agent = ?2 WHERE stage_id = ?1",
            params![stage_id, request.agent],
        )
        .map_err(backend)?;
        tx.commit().map_err(backend)?;
        Ok(Some(Claimed {
            job,
            stage_id,
            n: plan.n,
            name: plan.name,
            attempt: plan.attempt,
        }))
    }

    fn heartbeat(&mut self, stage_id: &str, expires_ms: i64) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE leases SET expires_ms = ?2 WHERE stage_id = ?1",
                params![stage_id, expires_ms],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn set_lease_process(
        &mut self,
        stage_id: &str,
        pid: u32,
        start: u64,
    ) -> Result<(), StoreError> {
        self.conn
            .execute(
                "UPDATE leases SET pid = ?2, pid_start = ?3 WHERE stage_id = ?1",
                params![stage_id, pid as i64, start as i64],
            )
            .map(|_| ())
            .map_err(backend)
    }

    fn lease_of_job(&self, job: &JobId) -> Result<Option<ExpiredLease>, StoreError> {
        self.conn
            .query_row(
                "SELECT job_id, stage_id, holder, pid, pid_start FROM leases
                 WHERE job_id = ?1 LIMIT 1",
                params![job.as_str()],
                lease_row,
            )
            .optional()
            .map_err(backend)
    }

    fn expired_leases(&self, now_ms: i64) -> Result<Vec<ExpiredLease>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT job_id, stage_id, holder, pid, pid_start FROM leases
                 WHERE expires_ms <= ?1 ORDER BY stage_id",
            )
            .map_err(backend)?;
        let rows = stmt
            .query_map(params![now_ms], lease_row)
            .map_err(backend)?;
        rows.map(|row| row.map_err(backend)).collect()
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

    fn new_job(id: &str, repo: Option<&str>) -> NewJob {
        NewJob {
            id: JobId::new(id),
            event_id: format!("E-{id}"),
            prompt: "p".into(),
            repo: repo.map(str::to_owned),
            priority: Priority::Normal,
            follow_up: None,
            submitted_at_ms: 7,
        }
    }

    fn claim_request(tag: &str, now_ms: i64) -> ClaimRequest {
        ClaimRequest {
            holder: "w1".into(),
            now_ms,
            ttl_ms: 1000,
            stage_id: format!("S-{tag}"),
            event_ids: [format!("EP-{tag}"), format!("EC-{tag}")],
            require_triage: false,
            agent: "claude".into(),
            caps: pizza_delivery_core::Caps::default(),
        }
    }

    fn kinds(s: &SqliteStore, job: &str) -> Result<Vec<&'static str>, StoreError> {
        Ok(s.read_timeline(&JobId::new(job))?
            .iter()
            .map(|e| e.payload.kind())
            .collect())
    }

    fn job_with(id: &str, repo: &str, priority: Priority, at: i64) -> NewJob {
        NewJob {
            repo: Some(repo.to_owned()),
            priority,
            follow_up: None,
            submitted_at_ms: at,
            ..new_job(id, None)
        }
    }

    fn claim_with(tag: &str, caps: pizza_delivery_core::Caps) -> ClaimRequest {
        ClaimRequest {
            caps,
            ..claim_request(tag, 100)
        }
    }

    #[test]
    fn the_highest_priority_job_is_claimed_first_then_the_earliest(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(job_with("J1", "/a", Priority::Normal, 1))?;
        s.enqueue_job(job_with("J2", "/b", Priority::High, 9))?;
        s.enqueue_job(job_with("J3", "/c", Priority::High, 5))?;
        let wide = pizza_delivery_core::Caps {
            global: 9,
            per_agent: 9,
            per_repo: 9,
        };
        let first = s.claim_stage(claim_with("a", wide))?.map(|c| c.job.id);
        let second = s.claim_stage(claim_with("b", wide))?.map(|c| c.job.id);
        let third = s.claim_stage(claim_with("c", wide))?.map(|c| c.job.id);
        assert_eq!(
            [first, second, third],
            [
                Some(JobId::new("J3")),
                Some(JobId::new("J2")),
                Some(JobId::new("J1"))
            ]
        );
        Ok(())
    }

    #[test]
    fn the_global_cap_stops_further_claims_until_a_lease_is_released(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        for (i, repo) in ["/a", "/b", "/c"].into_iter().enumerate() {
            s.enqueue_job(job_with(&format!("J{i}"), repo, Priority::Normal, i as i64))?;
        }
        let caps = pizza_delivery_core::Caps {
            global: 2,
            per_agent: 9,
            per_repo: 9,
        };
        assert!(s.claim_stage(claim_with("a", caps))?.is_some());
        let second = s.claim_stage(claim_with("b", caps))?;
        assert!(second.is_some());
        assert!(s.claim_stage(claim_with("c", caps))?.is_none());
        let released = NewEvent {
            id: "ER".into(),
            job_id: JobId::new("J0"),
            stage_id: Some("S-a".into()),
            ts_ms: 200,
            actor: Actor::Daemon,
            payload: EventPayload::LeaseReleased {
                holder: "w1".into(),
            },
        };
        s.append_event(released)?;
        assert!(s.claim_stage(claim_with("d", caps))?.is_some());
        Ok(())
    }

    #[test]
    fn the_per_repo_cap_lets_other_repos_through() -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(job_with("J1", "/same", Priority::High, 1))?;
        s.enqueue_job(job_with("J2", "/same", Priority::High, 2))?;
        s.enqueue_job(job_with("J3", "/other", Priority::Low, 3))?;
        let caps = pizza_delivery_core::Caps {
            global: 9,
            per_agent: 9,
            per_repo: 1,
        };
        let ids: Vec<_> = ["a", "b", "c"]
            .into_iter()
            .map(|t| {
                s.claim_stage(claim_with(t, caps))
                    .map(|c| c.map(|c| c.job.id))
            })
            .collect::<Result<_, _>>()?;
        assert_eq!(
            ids,
            vec![Some(JobId::new("J1")), Some(JobId::new("J3")), None]
        );
        Ok(())
    }

    #[test]
    fn the_per_agent_cap_counts_only_that_agents_running_stages(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(job_with("J1", "/a", Priority::Normal, 1))?;
        s.enqueue_job(job_with("J2", "/b", Priority::Normal, 2))?;
        let caps = pizza_delivery_core::Caps {
            global: 9,
            per_agent: 1,
            per_repo: 9,
        };
        assert!(s.claim_stage(claim_with("a", caps))?.is_some());
        assert!(s.claim_stage(claim_with("b", caps))?.is_none());
        let other = ClaimRequest {
            agent: "codex".into(),
            ..claim_with("c", caps)
        };
        assert!(s.claim_stage(other)?.is_some());
        Ok(())
    }

    #[test]
    fn enqueue_then_list_and_timeline() -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1", None))?;
        s.enqueue_job(new_job("J2", None))?;
        let ids: Vec<_> = s.list_jobs()?.into_iter().map(|j| j.id).collect();
        assert_eq!(ids, vec![JobId::new("J1"), JobId::new("J2")]);
        let tl = s.read_timeline(&JobId::new("J1"))?;
        assert_eq!(tl.len(), 1);
        assert_eq!(tl[0].payload.kind(), "job-submitted");
        Ok(())
    }

    #[test]
    fn jobs_without_a_repo_are_never_claimed() -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1", None))?;
        assert_eq!(s.claim_stage(claim_request("a", 100))?, None);
        Ok(())
    }

    #[test]
    fn claiming_plans_the_stage_ledgers_the_claim_and_runs_the_job(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1", Some("/r")))?;

        let claimed = s.claim_stage(claim_request("a", 100))?;

        let claimed = claimed.ok_or("nothing claimed")?;
        assert_eq!((claimed.n, claimed.attempt), (1, 1));
        assert_eq!(claimed.job.state, JobState::Running);
        assert_eq!(
            kinds(&s, "J1")?,
            vec!["job-submitted", "stage-planned", "stage-claimed"]
        );
        assert_eq!(s.claim_stage(claim_request("b", 101))?, None);
        Ok(())
    }

    #[test]
    fn an_unrenewed_lease_expires_and_a_heartbeat_keeps_it_alive(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1", Some("/r")))?;
        let claimed = s.claim_stage(claim_request("a", 100))?.ok_or("no claim")?;
        assert!(s.expired_leases(1099)?.is_empty());

        s.heartbeat(&claimed.stage_id, 5000)?;

        assert!(s.expired_leases(4999)?.is_empty());
        assert_eq!(s.expired_leases(5000)?.len(), 1);
        Ok(())
    }

    #[test]
    fn releasing_a_lease_removes_it_and_a_retried_stage_is_reclaimed_as_attempt_two(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (_d, mut s) = store()?;
        s.enqueue_job(new_job("J1", Some("/r")))?;
        let claimed = s.claim_stage(claim_request("a", 100))?.ok_or("no claim")?;
        let event = |id: &str, payload| NewEvent {
            id: id.into(),
            job_id: JobId::new("J1"),
            stage_id: Some(claimed.stage_id.clone()),
            ts_ms: 200,
            actor: Actor::Daemon,
            payload,
        };

        s.append_event(event(
            "E1",
            EventPayload::LeaseExpired {
                holder: "w1".into(),
            },
        ))?;
        s.append_event(event(
            "E2",
            EventPayload::StageRetried {
                reason: "lost".into(),
                cause: Default::default(),
            },
        ))?;

        assert!(s.expired_leases(i64::MAX)?.is_empty());
        let again = s
            .claim_stage(claim_request("b", 300))?
            .ok_or("no reclaim")?;
        assert_eq!((again.stage_id, again.attempt), (claimed.stage_id, 2));
        Ok(())
    }
}
