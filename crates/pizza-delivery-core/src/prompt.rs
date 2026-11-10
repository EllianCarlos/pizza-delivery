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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_carries_autonomy_preamble_task_and_handoff_location() {
        let prompt = render_stage_prompt("J1", "implement", "fix the bug", "/a/handoff.md");
        assert!(prompt.starts_with("You are running unattended. Never ask the human"));
        assert!(prompt.contains("fix the bug"));
        assert!(prompt.contains("/a/handoff.md"));
        assert!(prompt.contains("## Instructions for the next stage"));
    }
}
