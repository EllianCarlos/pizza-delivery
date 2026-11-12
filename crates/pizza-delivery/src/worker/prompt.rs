use super::*;

impl Worker<'_> {
    pub(super) fn templates(&self) -> Templates {
        PIPELINE_STAGES
            .iter()
            .fold(Templates::embedded(), |templates, name| {
                let text = std::fs::read_to_string(
                    self.config
                        .data_dir
                        .join("prompts")
                        .join(format!("{name}.md")),
                );
                match text {
                    Ok(text) => templates
                        .clone()
                        .with_override(name, &text)
                        .unwrap_or(templates),
                    Err(_) => templates,
                }
            })
    }

    pub(super) fn read_data(&self, relative: &str) -> Option<String> {
        std::fs::read_to_string(self.config.data_dir.join(relative)).ok()
    }

    pub(super) fn render_prompt(
        &self,
        claimed: &Claimed,
        run: &StageRun,
    ) -> Result<Option<Rendered>> {
        let events = self.with_store(|s| s.read_timeline(&claimed.job.id))?;
        let previous = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::HandoffWritten { path }
                if e.stage_id.as_deref() != Some(claimed.stage_id.as_str()) =>
            {
                self.read_data(path)
            }
            _ => None,
        });
        let report = events.iter().rev().find_map(|e| match &e.payload {
            EventPayload::ArtifactRecorded { kind, path, .. } if kind == "review-report" => {
                self.read_data(path)
            }
            _ => None,
        });
        let artifact_dir = self.config.data_dir.join(&run.relative);
        Ok(render(
            &self.templates(),
            &Vars {
                job: claimed.job.id.as_str(),
                stage: &claimed.name,
                task: &claimed.job.prompt,
                handoff_path: &run.handoff_path.to_string_lossy(),
                artifact_dir: &artifact_dir.to_string_lossy(),
                spec_dir: &spec_dir(&claimed.job.id),
                previous_handoff: previous.as_deref(),
                review_report: report.as_deref(),
                auto_at: self.config.gate.auto_at,
            },
        ))
    }
}
