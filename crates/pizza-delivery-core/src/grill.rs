use crate::triage::Feedback;

pub const DEFAULT_ROUND_CAP: u32 = 3;
pub const LOW_CONFIDENCE: u8 = 60;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assessment {
    pub rounds: u32,
    pub dropped_rounds: u32,
    pub feedback: Vec<Feedback>,
}

#[derive(Default)]
struct Entry {
    question: String,
    answer: String,
    confidence: Option<u8>,
    alternatives: Vec<String>,
    impact: Option<String>,
}

impl Entry {
    fn unanswerable(&self) -> bool {
        let answer = self.answer.trim().trim_end_matches('.').to_lowercase();
        answer.is_empty() || answer == "unanswerable"
    }

    fn feedback(&self) -> Option<Feedback> {
        let unanswerable = self.unanswerable();
        let low = self.confidence.is_some_and(|c| c < LOW_CONFIDENCE);
        (unanswerable || low).then(|| Feedback {
            kind: if unanswerable {
                "question"
            } else {
                "assumption"
            }
            .to_owned(),
            severity: if unanswerable { "medium" } else { "low" }.to_owned(),
            text: if unanswerable {
                self.question.clone()
            } else {
                format!("{} -> {}", self.question, self.answer.trim())
            },
            confidence: self.confidence,
            evidence: None,
            alternatives: self.alternatives.clone(),
            impact: self.impact.clone(),
        })
    }
}

fn is_round_heading(line: &str) -> bool {
    line.strip_prefix("## ")
        .is_some_and(|t| t.trim().to_lowercase().starts_with("round"))
}

fn parse_rounds(transcript: &str) -> Vec<Vec<Entry>> {
    transcript.lines().fold(Vec::new(), |mut rounds, line| {
        let line = line.trim();
        if is_round_heading(line) {
            rounds.push(Vec::new());
            return rounds;
        }
        let Some(round) = rounds.last_mut() else {
            return rounds;
        };
        let bullet = line.strip_prefix("- ").unwrap_or(line);
        if let Some(question) = bullet.strip_prefix("Q:") {
            round.push(Entry {
                question: question.trim().to_owned(),
                ..Entry::default()
            });
        } else if let Some(entry) = round.last_mut() {
            if let Some(answer) = bullet.strip_prefix("A:") {
                entry.answer = answer.trim().to_owned();
            } else if let Some(value) = bullet.strip_prefix("confidence:") {
                entry.confidence = value.trim().parse::<u8>().ok().map(|c| c.min(100));
            } else if let Some(value) = bullet.strip_prefix("alternatives:") {
                entry.alternatives = value
                    .split(',')
                    .map(|a| a.trim().to_owned())
                    .filter(|a| !a.is_empty())
                    .collect();
            } else if let Some(value) = bullet.strip_prefix("impact:") {
                entry.impact = Some(value.trim().to_owned()).filter(|i| !i.is_empty());
            }
        }
        rounds
    })
}

pub fn assess(transcript: &str, cap: u32) -> Assessment {
    let rounds = parse_rounds(transcript);
    let total = u32::try_from(rounds.len()).unwrap_or(u32::MAX);
    let kept = total.min(cap);
    let dropped = total - kept;
    let mut feedback: Vec<Feedback> = rounds
        .iter()
        .take(kept as usize)
        .flatten()
        .filter_map(Entry::feedback)
        .collect();
    if dropped > 0 {
        feedback.push(Feedback {
            kind: "risk".to_owned(),
            severity: "medium".to_owned(),
            text: format!(
                "self-grill stopped at the round cap of {cap}; {dropped} further round(s) were not considered"
            ),
            confidence: None,
            evidence: None,
            alternatives: Vec::new(),
            impact: Some("open questions may remain in the spec".to_owned()),
        });
    }
    Assessment {
        rounds: kept,
        dropped_rounds: dropped,
        feedback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &str = "# Grill\n\n## Round 1\n- Q: Which db?\n  A: sqlite\n  confidence: 90\n- Q: Which port?\n  A: 8080\n  confidence: 30\n  alternatives: 80, 9000\n  impact: clients must reconfigure\n\n## Round 2\n- Q: Who owns billing?\n  A: unanswerable\n\n## Round 3\n- Q: Cache?\n  A: none\n  confidence: 80\n\n## Round 4\n- Q: Never reached?\n  A: x\n  confidence: 10\n";

    #[test]
    fn rounds_beyond_the_cap_are_dropped_and_recorded_as_a_risk() {
        let assessment = assess(TRANSCRIPT, 3);
        assert_eq!((assessment.rounds, assessment.dropped_rounds), (3, 1));
        let risk = assessment.feedback.iter().find(|f| f.kind == "risk");
        assert!(risk.is_some_and(|r| r.text.contains("round cap of 3")));
        assert!(!assessment
            .feedback
            .iter()
            .any(|f| f.text.contains("Never reached")));
    }

    #[test]
    fn a_low_confidence_answer_becomes_an_assumption_with_alternatives_and_impact() {
        let assessment = assess(TRANSCRIPT, 3);
        let port = assessment
            .feedback
            .iter()
            .find(|f| f.text.contains("Which port?"));
        assert_eq!(
            port,
            Some(&Feedback {
                kind: "assumption".into(),
                severity: "low".into(),
                text: "Which port? -> 8080".into(),
                confidence: Some(30),
                evidence: None,
                alternatives: vec!["80".into(), "9000".into()],
                impact: Some("clients must reconfigure".into()),
            })
        );
    }

    #[test]
    fn an_unanswerable_question_becomes_a_question_entry() {
        let assessment = assess(TRANSCRIPT, 3);
        let billing = assessment
            .feedback
            .iter()
            .find(|f| f.text == "Who owns billing?");
        assert_eq!(billing.map(|f| f.kind.as_str()), Some("question"));
    }

    #[test]
    fn confident_answers_raise_nothing() {
        let assessment = assess(TRANSCRIPT, 3);
        assert!(!assessment
            .feedback
            .iter()
            .any(|f| f.text.contains("Which db?") || f.text.contains("Cache?")));
    }

    #[test]
    fn a_missing_or_malformed_transcript_never_blocks() {
        assert_eq!(
            assess("", 3),
            Assessment {
                rounds: 0,
                dropped_rounds: 0,
                feedback: vec![]
            }
        );
        assert_eq!(assess("just prose", 3).rounds, 0);
    }
}
