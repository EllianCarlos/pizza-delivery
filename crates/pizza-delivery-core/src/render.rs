use serde_json::json;

use crate::api::JobView;
use crate::event::Event;
use crate::id::JobId;

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

pub fn render_timeline(job: &JobView, events: &[Event], mode: OutputMode) -> String {
    match mode {
        OutputMode::Json => line(&json!({
            "id": job.id,
            "short_id": job.short_id,
            "state": job.state,
            "prompt": job.prompt,
            "submitted_at_ms": job.submitted_at_ms,
            "events": events,
        })),
        OutputMode::Human => {
            let head = format!(
                "{}  {}\n{}\n\n",
                job.id,
                job.state.as_str(),
                one_line(&job.prompt)
            );
            let rows: String = events
                .iter()
                .map(|e| {
                    format!(
                        "{:>3}  {:<14}  {}\n",
                        e.seq,
                        e.payload.kind(),
                        e.actor.as_str()
                    )
                })
                .collect();
            format!("{head}{rows}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobState;

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
