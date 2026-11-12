use serde_json::json;

use crate::api::{JobView, Response};
use crate::event::Event;
use crate::id::JobId;
use crate::inbox::InboxEntry;
use crate::quota::QuotaView;
use crate::stage::StageView;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    Human,
    Json,
}

impl OutputMode {
    pub fn resolve(json_flag: bool, stdout_is_tty: bool) -> Self {
        if json_flag || !stdout_is_tty {
            OutputMode::Json
        } else {
            OutputMode::Human
        }
    }
}

fn line(value: &serde_json::Value) -> String {
    format!("{}\n", serde_json::to_string(value).unwrap_or_default())
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn clip(text: &str, max: usize) -> String {
    let flat = one_line(text);
    if flat.chars().count() <= max {
        flat
    } else {
        let head: String = flat.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

pub fn render_submitted(id: &JobId, short_id: &str, mode: OutputMode) -> String {
    match mode {
        OutputMode::Human => format!("{short_id}\n"),
        OutputMode::Json => line(&json!({"id": id, "short_id": short_id})),
    }
}

fn status_text(entry: &InboxEntry) -> String {
    match &entry.answered_by {
        Some(by) => format!(
            "answered by {}",
            by.as_str().chars().take(8).collect::<String>()
        ),
        None => entry.status.as_str().to_owned(),
    }
}

pub fn render_inbox(entries: &[InboxEntry], mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => entries
            .iter()
            .map(|e| line(&serde_json::to_value(e).unwrap_or_default()))
            .collect(),
        OutputMode::Human if entries.is_empty() => "inbox is empty\n".to_owned(),
        OutputMode::Human => {
            let width = entries
                .iter()
                .map(|e| e.short_id.len())
                .max()
                .unwrap_or(2)
                .max(2);
            let rows: String = entries
                .iter()
                .map(|e| {
                    format!(
                        "{:<width$}  {:<8}  {:<8}  {}  ({})\n",
                        e.short_id,
                        e.severity,
                        e.kind,
                        clip(&e.text, 60),
                        status_text(e)
                    )
                })
                .collect();
            format!(
                "{:<width$}  {:<8}  {:<8}  TEXT\n{rows}",
                "ID", "SEVERITY", "KIND"
            )
        }
    }
}

pub fn render_inbox_entry(entry: &InboxEntry, mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => line(&serde_json::to_value(entry).unwrap_or_default()),
        OutputMode::Human => {
            let confidence = entry
                .confidence
                .map_or_else(|| "-".to_owned(), |c| format!("{c}%"));
            let alternatives = if entry.alternatives.is_empty() {
                "-".to_owned()
            } else {
                entry.alternatives.join(", ")
            };
            format!(
                "entry        {}\njob          {}\nkind         {}\nseverity     {}\nconfidence   {}\nalternatives {}\nimpact       {}\nstatus       {}\n\n{}\n",
                entry.id,
                entry.job,
                entry.kind,
                entry.severity,
                confidence,
                alternatives,
                entry.impact.as_deref().unwrap_or("-"),
                status_text(entry),
                entry.text
            )
        }
    }
}

pub fn render_dismissed(entries: &[String], mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => line(&json!({"dismissed": entries})),
        OutputMode::Human => format!("dismissed {}\n", entries.len()),
    }
}

pub fn render_replied(id: &JobId, short_id: &str, answered: &[String], mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => line(&json!({"id": id, "short_id": short_id, "answered": answered})),
        OutputMode::Human => format!("{short_id}\n"),
    }
}

pub fn render_jobs(jobs: &[JobView], mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => jobs
            .iter()
            .map(|j| line(&serde_json::to_value(j).unwrap_or_default()))
            .collect(),
        OutputMode::Human if jobs.is_empty() => "no jobs\n".to_owned(),
        OutputMode::Human => {
            let width = jobs
                .iter()
                .map(|j| j.short_id.len())
                .max()
                .unwrap_or(2)
                .max(2);
            let header = format!("{:<width$}  {:<8}  PROMPT\n", "ID", "STATE");
            let rows: String = jobs
                .iter()
                .map(|j| {
                    format!(
                        "{:<width$}  {:<8}  {}\n",
                        j.short_id,
                        j.state.as_str(),
                        clip(&j.prompt, 60)
                    )
                })
                .collect();
            format!("{header}{rows}")
        }
    }
}

pub fn render_quota(agents: &[QuotaView], now_ms: i64, mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => agents
            .iter()
            .map(|a| line(&serde_json::to_value(a).unwrap_or_default()))
            .collect(),
        OutputMode::Human if agents.is_empty() => "no quota signals yet\n".to_owned(),
        OutputMode::Human => {
            let rows: String = agents
                .iter()
                .map(|a| {
                    let used = a
                        .utilization_permille
                        .map_or_else(|| "-".to_owned(), |u| format!("{}%", u / 10));
                    let reset = match (a.resets_at_ms, a.reset_known) {
                        (None, _) => "-".to_owned(),
                        (Some(at), known) => format!(
                            "in {}s{}",
                            (at - now_ms).max(0) / 1000,
                            if known { "" } else { " (default backoff)" }
                        ),
                    };
                    format!("{:<10}  {:<9}  {:>5}  {}\n", a.agent, a.state, used, reset)
                })
                .collect();
            format!(
                "{:<10}  {:<9}  {:>5}  RESET\n{rows}",
                "AGENT", "STATE", "USED"
            )
        }
    }
}

fn count(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |n| n.to_string())
}

fn stage_line(stage: &StageView) -> String {
    let usage = stage.usage.as_ref();
    format!(
        "stage {} {:<10} {:<9} attempt {}  in {} out {} ms {}\n",
        stage.n,
        stage.name,
        stage.state.as_str(),
        stage.attempt,
        count(usage.and_then(|u| u.input_tokens)),
        count(usage.and_then(|u| u.output_tokens)),
        count(usage.and_then(|u| u.wall_ms)),
    )
}

pub fn render_timeline(
    job: &JobView,
    stages: &[StageView],
    events: &[Event],
    mode: OutputMode,
) -> String {
    match mode {
        OutputMode::Json => line(&json!({
            "id": job.id,
            "short_id": job.short_id,
            "state": job.state,
            "prompt": job.prompt,
            "submitted_at_ms": job.submitted_at_ms,
            "stages": stages,
            "events": events,
        })),
        OutputMode::Human => {
            let head = format!(
                "{}  {}\n{}\n\n",
                job.id,
                job.state.as_str(),
                one_line(&job.prompt)
            );
            let stage_rows: String = stages.iter().map(stage_line).collect();
            let stage_rows = if stage_rows.is_empty() {
                stage_rows
            } else {
                format!("{stage_rows}\n")
            };
            let rows: String = events
                .iter()
                .map(|e| {
                    format!(
                        "{:>3}  {:<17}  {}\n",
                        e.seq,
                        e.payload.kind(),
                        e.actor.as_str()
                    )
                })
                .collect();
            format!("{head}{stage_rows}{rows}")
        }
    }
}

fn stamp(ts_ms: i64) -> String {
    let secs = ts_ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

pub fn render_artifacts(response: &Response, mode: OutputMode) -> String {
    let Response::ArtifactItems { items } = response else {
        return String::new();
    };
    if mode == OutputMode::Json {
        return line(&json!(response));
    }
    items
        .iter()
        .map(|a| format!("{} {} {} {}\n", stamp(a.ts_ms), a.short_id, a.kind, a.path))
        .collect()
}

pub fn render_events(response: &Response, mode: OutputMode) -> String {
    let Response::EventItems { items } = response else {
        return String::new();
    };
    if mode == OutputMode::Json {
        return line(&json!(response));
    }
    items
        .iter()
        .map(|e| {
            format!(
                "{} {} {} {}{}\n",
                stamp(e.ts_ms),
                e.job_id.as_str().get(..8).unwrap_or(e.job_id.as_str()),
                e.payload.kind(),
                e.actor.as_str(),
                e.stage_id
                    .as_deref()
                    .map(|s| format!(" {s}"))
                    .unwrap_or_default()
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobState;

    fn exhausted_claude() -> QuotaView {
        QuotaView {
            agent: "claude".into(),
            state: "exhausted".into(),
            utilization_permille: Some(1000),
            resets_at_ms: Some(65_000),
            reset_known: true,
        }
    }

    #[test]
    fn quota_renders_one_json_line_per_agent_and_a_table_for_humans() {
        let agents = [exhausted_claude()];
        let json = render_quota(&agents, 5_000, OutputMode::Json);
        let human = render_quota(&agents, 5_000, OutputMode::Human);
        let first: serde_json::Value = serde_json::from_str(json.trim()).unwrap_or_default();
        assert_eq!(first["agent"], "claude");
        assert_eq!(first["state"], "exhausted");
        assert!(human.contains("claude"));
        assert!(human.contains("100%"));
        assert!(human.contains("in 60s"));
        assert_eq!(
            render_quota(&[], 0, OutputMode::Human),
            "no quota signals yet\n"
        );
    }

    fn view(short: &str, prompt: &str) -> JobView {
        JobView {
            id: JobId::new(format!("{short}XXXXXXXXXXXXXXXXXX")),
            short_id: short.into(),
            state: JobState::Queued,
            prompt: prompt.into(),
            submitted_at_ms: 0,
        }
    }

    #[test]
    fn human_timeline_lists_stages_with_unknown_usage_as_dashes() {
        use crate::stage::{StageState, StageView};
        let stage = StageView {
            id: "S1".into(),
            n: 1,
            name: "implement".into(),
            state: StageState::Succeeded,
            attempt: 1,
            usage: Some(crate::usage::Usage {
                output_tokens: Some(8),
                ..Default::default()
            }),
        };
        let text = render_timeline(&view("AB", "p"), &[stage], &[], OutputMode::Human);
        assert!(text.contains("stage 1 implement  succeeded attempt 1  in - out 8 ms -"));
    }

    #[test]
    fn output_mode_is_json_when_piped_or_flagged() {
        assert_eq!(OutputMode::resolve(false, true), OutputMode::Human);
        assert_eq!(OutputMode::resolve(false, false), OutputMode::Json);
        assert_eq!(OutputMode::resolve(true, true), OutputMode::Json);
    }

    #[test]
    fn human_table_has_header_and_one_row_per_job() {
        let out = render_jobs(
            &[view("01ABCDEF", "first\nline"), view("01ABCDEG", "b")],
            OutputMode::Human,
        );
        assert_eq!(
            out,
            "ID        STATE     PROMPT\n01ABCDEF  queued    first line\n01ABCDEG  queued    b\n"
        );
    }

    #[test]
    fn json_lines_are_one_object_per_job() {
        let out = render_jobs(
            &[view("01ABCDEF", "a"), view("01ABCDEG", "b")],
            OutputMode::Json,
        );
        assert_eq!(out.lines().count(), 2);
        assert!(out.lines().all(|l| l.starts_with('{') && l.ends_with('}')));
    }

    #[test]
    fn long_prompts_are_clipped_in_the_table() {
        let out = render_jobs(&[view("01ABCDEF", &"w ".repeat(100))], OutputMode::Human);
        assert!(out.contains('…'));
    }

    #[test]
    fn submitted_prints_only_the_short_id_for_humans() {
        let id = JobId::new("01ABCDEFGHJKMNPQRSTVWXYZ00");
        assert_eq!(
            render_submitted(&id, "01ABCDEF", OutputMode::Human),
            "01ABCDEF\n"
        );
        assert_eq!(
            render_submitted(&id, "01ABCDEF", OutputMode::Json),
            "{\"id\":\"01ABCDEFGHJKMNPQRSTVWXYZ00\",\"short_id\":\"01ABCDEF\"}\n"
        );
    }
}
