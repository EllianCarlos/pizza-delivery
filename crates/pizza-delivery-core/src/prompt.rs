use crate::handoff;

pub const TEMPLATE_NAME: &str = "stage";
pub const TEMPLATE_VERSION: u32 = 2;

const PREAMBLE: &str = "You are running unattended. Never ask the human anything. Wherever a skill or instruction says to ask, interview or check with the user, take the recommended answer and note it as an assumption in the handoff.";

fn calibration(auto_at: u8) -> String {
    format!(
        "Every bullet under Assumptions, Open questions and Risks must end with [confidence: 0-100; alternatives: a, b; impact: low|medium|high|irreversible; evidence: <source>], where evidence cites the file, command or document you checked. When your confidence is {auto_at} or higher, take the recommended answer and keep going; only low-confidence, high-impact items are escalated to the human, and a bullet without this bracket is escalated too."
    )
}

pub fn render_stage_prompt(
    job_id: &str,
    stage: &str,
    task: &str,
    handoff_path: &str,
    auto_at: u8,
) -> String {
    let calibration = calibration(auto_at);
    format!(
        "{PREAMBLE}\n\n{calibration}\n\nStage: {stage}\nJob: {job_id}\n\nTask:\n{task}\n\nWhen the work is done, write the handoff document to {handoff_path} using exactly this structure:\n\n{}",
        handoff::template(job_id, stage)
    )
}

pub fn with_interruption_note(prompt: &str, note: Option<&str>) -> String {
    match note {
        None => prompt.to_owned(),
        Some(note) => format!(
            "{prompt}\n\nA previous attempt of this stage was interrupted. Read this before continuing:\n\n{note}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interruption_note_is_absent_without_one_and_appended_with_one() {
        assert_eq!(with_interruption_note("base", None), "base");
        let prompt = with_interruption_note("base", Some("# Interruption note"));
        assert!(prompt.starts_with("base"));
        assert!(prompt.contains("A previous attempt of this stage was interrupted"));
        assert!(prompt.ends_with("# Interruption note"));
    }

    #[test]
    fn the_prompt_carries_autonomy_preamble_task_and_handoff_location() {
        let prompt = render_stage_prompt("J1", "implement", "fix the bug", "/a/handoff.md", 80);
        assert!(prompt.starts_with("You are running unattended. Never ask the human"));
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("/a/handoff.md"));
        assert!(prompt.contains("## Instructions for the next stage"));
    }

    #[test]
    fn the_prompt_asks_for_calibrated_metadata_with_the_configured_threshold() {
        let prompt = render_stage_prompt("J1", "grill", "t", "/h.md", 73);
        assert!(prompt.contains("[confidence: 0-100; alternatives: a, b; impact: low|medium|high|irreversible; evidence: <source>]"));
        assert!(prompt.contains("confidence is 73 or higher"));
        assert_eq!(TEMPLATE_VERSION, 2);
    }
}
