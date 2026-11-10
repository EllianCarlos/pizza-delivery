mod harness;

use harness::{Harness, LedgerRow, TestResult};

const GRILL_FIVE_ROUNDS: &str = "## Round 1\n- Q: which db?\n  A: sqlite\n  confidence: 90\n## Round 2\n- Q: which port?\n  A: 8080\n  confidence: 30\n  alternatives: 80, 9000\n  impact: clients reconfigure\n## Round 3\n- Q: who pays?\n  A: unanswerable\n## Round 4\n- Q: late question?\n  A: x\n  confidence: 10\n## Round 5\n- Q: later question?\n  A: y\n  confidence: 10\n";
const GRILL_CLEAN: &str = "## Round 1\n- Q: which db?\n  A: sqlite\n  confidence: 90\n";
const BLOCKING: &str =
    "## Findings\n- [blocking] null deref in parser\n- [non-blocking] rename helper\n";
const CLEAN: &str = "## Findings\nNone\n";
const SPEC: &str =
    "## Problem\np\n\n## Solution\ns\n\n## Acceptance criteria\n- a\n\n## Seams\n- cli\n";
const GOOD_TICKETS: &[(&str, &str)] = &[
    ("t1", "---\nid: t1\nblocked_by: []\n---\nbody\n"),
    ("t2", "---\nid: t2\nblocked_by: [t1]\n---\nbody\n"),
];
const CYCLIC_TICKETS: &[(&str, &str)] = &[
    ("t1", "---\nid: t1\nblocked_by: [t2]\n---\nbody\n"),
    ("t2", "---\nid: t2\nblocked_by: [t1]\n---\nbody\n"),
];
const NO_FEEDBACK: &str = "none";

struct Plan<'a> {
    grill: &'a str,
    review_first: &'a str,
    review_after_fix: &'a str,
    tickets: &'a [(&'a str, &'a str)],
    assumptions: &'a str,
}

impl Default for Plan<'_> {
    fn default() -> Self {
        Self {
            grill: GRILL_CLEAN,
            review_first: CLEAN,
            review_after_fix: CLEAN,
            tickets: GOOD_TICKETS,
            assumptions: NO_FEEDBACK,
        }
    }
}

fn agent(plan: &Plan<'_>) -> String {
    let tickets: String = plan
        .tickets
        .iter()
        .map(|(name, text)| format!("cat > \"docs/specs/$JOB/tickets/{name}.md\" <<'T'\n{text}T\n"))
        .collect();
    format!(
        r#"#!/bin/sh
STAGE_DIR=$(dirname "$PIZZA_DELIVERY_HANDOFF")
STAGE=$(basename "$STAGE_DIR" | sed 's/^[0-9]*-//')
JOB=$(basename "$(dirname "$(dirname "$STAGE_DIR")")")
case "$STAGE" in
grill)
cat > "$STAGE_DIR/grill.md" <<'G'
{grill}G
;;
spec)
mkdir -p "docs/specs/$JOB/tickets"
cat > "docs/specs/$JOB/spec.md" <<'S'
{spec}S
{tickets};;
fix)
touch fixed.txt
;;
review)
if [ -f fixed.txt ]; then
cat > "$STAGE_DIR/review.md" <<'R'
{after}R
else
cat > "$STAGE_DIR/review.md" <<'R'
{first}R
fi
;;
esac
cat > "$PIZZA_DELIVERY_HANDOFF" <<'DOC'
---
job: J
stage: any
---

## Goal
do the thing

## What was done
did it

## Decisions made
none

## Assumptions
{assumptions}

## Open questions
none

## Files touched
none

## Instructions for the next stage
go on
DOC
echo '{{"type":"result","is_error":false}}'
"#,
        grill = plan.grill,
        spec = SPEC,
        tickets = tickets,
        first = plan.review_first,
        after = plan.review_after_fix,
        assumptions = plan.assumptions,
    )
}

fn rows(h: &Harness, id: &str, kind: &str) -> TestResult<Vec<LedgerRow>> {
    Ok(h.ledger()?
        .into_iter()
        .filter(|r| r.job_id == id && r.kind == kind)
        .collect())
}

fn stage_names(job: &serde_json::Value) -> Vec<String> {
    job["stages"]
        .as_array()
        .map(|stages| {
            stages
                .iter()
                .filter_map(|s| s["name"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn the_grill_never_blocks_and_stops_at_its_round_cap() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan {
        grill: GRILL_FIVE_ROUNDS,
        ..Plan::default()
    }))?;
    let id = h.submit_in_repo("fix the bug")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    let feedback = rows(&h, &id, "feedback-recorded")?;
    let texts: Vec<String> = feedback
        .iter()
        .filter_map(|r| r.payload["text"].as_str().map(str::to_owned))
        .collect();
    assert!(texts.iter().any(|t| t.contains("which port?")));
    assert!(texts.iter().any(|t| t == "who pays?"));
    assert!(!texts.iter().any(|t| t.contains("late question?")));
    assert!(texts.iter().any(|t| t.contains("round cap of 3")));
    let port = feedback
        .iter()
        .find(|r| {
            r.payload["text"]
                .as_str()
                .is_some_and(|t| t.contains("which port?"))
        })
        .ok_or("no port entry")?;
    assert_eq!(port.payload["confidence"], 30);
    assert_eq!(
        port.payload["alternatives"],
        serde_json::json!(["80", "9000"])
    );
    assert_eq!(port.payload["impact"], "clients reconfigure");
    Ok(())
}

#[test]
fn a_missing_grill_transcript_does_not_block_the_job() -> TestResult {
    let script = agent(&Plan::default()).replace(
        "cat > \"$STAGE_DIR/grill.md\" <<'G'",
        "cat > /dev/null <<'G'",
    );
    let h = Harness::with_agent(&script)?;
    let id = h.submit_in_repo("fix the bug")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    Ok(())
}

#[test]
fn the_spec_stage_commits_to_a_valid_graph_and_indexes_the_files() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan::default()))?;
    let id = h.submit_in_repo("add a toggle")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    assert_eq!(
        stage_names(&job),
        vec!["grill", "spec", "implement", "review"]
    );
    let artifacts = rows(&h, &id, "artifact-recorded")?;
    let kinds = |kind: &str| {
        artifacts
            .iter()
            .filter(|r| r.payload["kind"] == kind)
            .count()
    };
    assert_eq!(kinds("spec"), 1);
    assert_eq!(kinds("ticket"), 2);
    Ok(())
}

#[test]
fn a_cyclic_ticket_graph_is_rejected_then_fails_the_job() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan {
        tickets: CYCLIC_TICKETS,
        ..Plan::default()
    }))?;
    let id = h.submit_in_repo("add a toggle")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "failed");
    let retried = rows(&h, &id, "stage-retried")?;
    assert_eq!(retried.len(), 1);
    assert!(retried[0].payload["reason"]
        .as_str()
        .is_some_and(|r| r.contains("invalid spec") && r.contains("cycle")));
    assert!(!stage_names(&job).contains(&"implement".to_owned()));
    Ok(())
}

#[test]
fn a_spec_missing_a_required_section_is_rejected() -> TestResult {
    let script = agent(&Plan::default()).replace("## Seams\n- cli\n", "");
    let h = Harness::with_agent(&script)?;
    let id = h.submit_in_repo("add a toggle")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "failed");
    let retried = rows(&h, &id, "stage-retried")?;
    assert!(retried[0].payload["reason"]
        .as_str()
        .is_some_and(|r| r.contains("seams")));
    Ok(())
}

#[test]
fn a_fixed_finding_is_fixed_once_and_re_reviewed_clean() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan {
        review_first: BLOCKING,
        ..Plan::default()
    }))?;
    let id = h.submit_in_repo("fix the bug")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    assert_eq!(
        stage_names(&job),
        vec!["grill", "implement", "review", "fix", "review"]
    );
    assert_eq!(rows(&h, &id, "plan-amended")?.len(), 1);
    assert!(!rows(&h, &id, "feedback-recorded")?
        .iter()
        .any(|r| r.payload["kind"] == "risk"));
    Ok(())
}

#[test]
fn the_review_fix_loop_stops_after_two_rounds_and_records_the_leftovers() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan {
        review_first: BLOCKING,
        review_after_fix: BLOCKING,
        ..Plan::default()
    }))?;
    let id = h.submit_in_repo("fix the bug")?;

    let job = h.wait_for_end(&id)?;

    assert_eq!(job["state"], "completed");
    assert_eq!(
        stage_names(&job),
        vec![
            "grill",
            "implement",
            "review",
            "fix",
            "review",
            "fix",
            "review"
        ]
    );
    assert_eq!(rows(&h, &id, "plan-amended")?.len(), 2);
    let risks: Vec<_> = rows(&h, &id, "feedback-recorded")?
        .into_iter()
        .filter(|r| r.payload["kind"] == "risk")
        .collect();
    assert_eq!(risks.len(), 1);
    assert_eq!(risks[0].payload["severity"], "high");
    assert!(risks[0].payload["text"]
        .as_str()
        .is_some_and(|t| t.contains("null deref in parser")));
    Ok(())
}

#[test]
fn feedback_in_a_handoff_is_ledgered_with_confidence_alternatives_and_impact() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan {
        assumptions: "- The API stays v1 [confidence: 70; alternatives: bump to v2, keep both; impact: clients break on v2]",
        ..Plan::default()
    }))?;
    let id = h.submit_in_repo("fix the bug")?;
    h.wait_for_end(&id)?;

    let entry = rows(&h, &id, "feedback-recorded")?
        .into_iter()
        .find(|r| r.payload["text"] == "The API stays v1")
        .ok_or("no feedback entry")?;

    assert_eq!(entry.actor, "agent");
    assert_eq!(entry.payload["kind"], "assumption");
    assert_eq!(entry.payload["confidence"], 70);
    assert_eq!(
        entry.payload["alternatives"],
        serde_json::json!(["bump to v2", "keep both"])
    );
    assert_eq!(entry.payload["impact"], "clients break on v2");
    Ok(())
}

#[test]
fn agent_invoked_events_carry_template_name_version_and_prompt_hash() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan::default()))?;
    let id = h.submit_in_repo("fix the bug")?;
    h.wait_for_end(&id)?;

    let invoked = rows(&h, &id, "agent-invoked")?;

    let templates: Vec<_> = invoked
        .iter()
        .map(|r| {
            r.payload["template"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(templates, vec!["grill", "implement", "review"]);
    for row in &invoked {
        assert_eq!(row.payload["version"], 1);
        assert!(row.payload["prompt_sha256"]
            .as_str()
            .is_some_and(|h| h.len() == 64));
    }
    Ok(())
}

#[test]
fn an_override_template_replaces_the_embedded_one_and_records_its_version() -> TestResult {
    let h = Harness::with_agent(&agent(&Plan::default()))?;
    let prompts = h.data_dir().join("prompts");
    std::fs::create_dir_all(&prompts)?;
    std::fs::write(
        prompts.join("implement.md"),
        "---\nversion: 9\n---\nCustom implement for {{task}}\n",
    )?;
    let id = h.submit_in_repo("fix the bug")?;
    h.wait_for_end(&id)?;

    let implement = rows(&h, &id, "agent-invoked")?
        .into_iter()
        .find(|r| r.payload["template"] == "implement")
        .ok_or("no implement invocation")?;

    assert_eq!(implement.payload["version"], 9);
    Ok(())
}
