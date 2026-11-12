mod harness;

use harness::{Harness, TestResult};
use pizza_delivery_core::tui::{resume_plan, Key, Model, Resume, ViewKind};
use pizza_delivery_core::view::EventsView;

fn harness() -> TestResult<Harness> {
    let script = harness::VALID_HANDOFF_AGENT.replace(
        "## Assumptions\nnone",
        "## Assumptions\n- The API stays v1 [confidence: 70; alternatives: bump to v2; impact: clients break]",
    );
    Harness::with_agent(&script)
}

fn events_since(h: &Harness, since: u64, limit: usize) -> TestResult<EventsView> {
    let value = h.get(&format!("/api/events?since={since}&limit={limit}"))?;
    Ok(serde_json::from_value(value)?)
}

#[test]
fn bare_invocation_off_a_tty_prints_usage_and_exits_2() -> TestResult {
    let h = Harness::new()?;
    let cli = h.run(&[])?;
    assert_eq!(cli.code, 2);
    assert!(cli.stdout.contains("Usage:"));
    assert!(cli.stdout.contains("tui"));
    Ok(())
}

#[test]
fn tui_subcommand_off_a_tty_is_a_usage_error() -> TestResult {
    let h = Harness::new()?;
    let cli = h.run(&["tui"])?;
    assert_eq!(cli.code, 2);
    assert!(cli.stderr.contains("interactive terminal"));
    Ok(())
}

#[test]
fn the_model_built_from_the_event_stream_agrees_with_the_daemon_views() -> TestResult {
    let h = harness()?;
    let id = h.submit_in_repo("build the thing")?;
    h.wait_for_end(&id)?;

    let mut model = Model::new();
    let all = events_since(&h, 0, 5000)?;
    model.apply(&all.events);

    let jobs = h.get("/api/jobs")?;
    let job = model.jobs().into_iter().next().ok_or("no job in model")?;
    assert_eq!(job.id.as_str(), jobs["jobs"][0]["id"].as_str().ok_or("id")?);
    assert_eq!(
        job.state.as_str(),
        jobs["jobs"][0]["state"].as_str().ok_or("state")?
    );

    let inbox = h.get("/api/inbox")?;
    assert_eq!(
        model.inbox().len(),
        inbox["entries"].as_array().ok_or("entries")?.len()
    );

    model.handle_key(Key::Char('2'));
    assert_eq!(model.view(), ViewKind::Board);
    let selected = model.selected_job().ok_or("nothing selected")?;
    assert_eq!(selected.id, job.id);
    Ok(())
}

#[test]
fn reconnect_resumes_from_the_last_sequence_when_the_daemon_still_has_it() -> TestResult {
    let h = harness()?;
    let id = h.submit_in_repo("build the thing")?;
    h.wait_for_end(&id)?;

    let all = events_since(&h, 0, 5000)?;
    let last = all.events.last().ok_or("no events")?;
    let probe = events_since(&h, last.cursor - 1, 1)?;
    assert_eq!(
        resume_plan(Some((last.cursor, &last.id)), &probe.events),
        Resume::From(last.cursor)
    );
    Ok(())
}

#[test]
fn reconnect_takes_a_snapshot_when_the_remembered_cursor_is_gone() -> TestResult {
    let h = harness()?;
    let id = h.submit_in_repo("build the thing")?;
    h.wait_for_end(&id)?;

    let all = events_since(&h, 0, 5000)?;
    let last = all.events.last().ok_or("no events")?;
    let beyond = last.cursor + 50;
    let probe = events_since(&h, beyond - 1, 1)?;
    assert_eq!(
        resume_plan(Some((beyond, "from-another-store")), &probe.events),
        Resume::Snapshot
    );
    let wrong_id = events_since(&h, last.cursor - 1, 1)?;
    assert_eq!(
        resume_plan(Some((last.cursor, "not-the-same-event")), &wrong_id.events),
        Resume::Snapshot
    );
    Ok(())
}
