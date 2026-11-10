use crate::handoff;

pub const TEMPLATE_NAME: &str = "stage";
pub const TEMPLATE_VERSION: u32 = 1;

const PREAMBLE: &str = "You are running unattended. Never ask the human anything. Wherever a skill or instruction says to ask, interview or check with the user, take the recommended answer and note it as an assumption in the handoff.";

pub fn render_stage_prompt(job_id: &str, stage: &str, task: &str, handoff_path: &str) -> String {
    format!(
        "{PREAMBLE}\n\nStage: {stage}\nJob: {job_id}\n\nTask:\n{task}\n\nWhen the work is done, write the handoff document to {handoff_path} using exactly this structure:\n\n{}",
        handoff::template(job_id, stage)
    )
}

pub fn with_interruption_note(prompt: &str, interrupted: Option<&str>) -> String {
    match interrupted {
        None => prompt.to_owned(),
        Some(agent) => format!(
            "{prompt}\n\nInterruption note: the previous run of this stage ({agent}) was interrupted by quota exhaustion before it finished. Its edits may already be present in the working tree. Restart the stage from its input above, check what exists, and finish the work."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interruption_note_names_the_agent_and_is_absent_without_one() {
        assert_eq!(with_interruption_note("p", None), "p");
        let noted = with_interruption_note("p", Some("claude"));
        assert!(noted.starts_with("p\n\nInterruption note:"));
        assert!(noted.contains("(claude)"));
    }

    #[test]
    fn the_prompt_carries_autonomy_preamble_task_and_handoff_location() {
        let prompt = render_stage_prompt("J1", "implement", "fix the bug", "/a/handoff.md");
        assert!(prompt.starts_with("You are running unattended. Never ask the human"));
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("/a/handoff.md"));
        assert!(prompt.contains("## Instructions for the next stage"));
    }
}
