use super::*;

fn install_memory(rig: &mut Rig) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let engram = rig.config.data_dir.join("engram");
    let mnemon = rig.config.data_dir.join("mnemon");
    for path in [&engram, &mnemon] {
        std::fs::write(path, FAKE_MEMORY_CLI)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    rig.config.memory = Some(MemorySetup { engram, mnemon });
    Ok(())
}

fn memory_store(rig: &Rig, name: &str) -> String {
    std::fs::read_to_string(rig.config.data_dir.join(format!("{name}.store")))
        .unwrap_or_default()
}

fn seed_memory(rig: &Rig, name: &str, line: &str) -> Result<()> {
    std::fs::write(
        rig.config.data_dir.join(format!("{name}.store")),
        format!("{line}\n"),
    )?;
    Ok(())
}

fn plan(lines: &'static str) -> Script {
    Script {
        handoff: Some(lines),
        stdout: OK_RESULT,
        ..Script::default()
    }
}

fn write_backs(rig: &Rig) -> Result<Vec<(String, String, String)>> {
    Ok(rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::WriteBack {
                store,
                topic,
                action,
                ..
            } => Some((store, topic, action)),
            _ => None,
        })
        .collect())
}

fn triple(store: &str, topic: &str, action: &str) -> (String, String, String) {
    (store.to_owned(), topic.to_owned(), action.to_owned())
}

const FIX_PLAN: &str =
    "{\"kind\":\"fix\",\"topic\":\"Login bug\",\"content\":\"hash before compare\"}\n";

#[test]
fn a_completed_job_saves_project_facts_to_engram_and_reconciles_them_by_job_tag() -> Result<()>
{
    let mut rig = Rig::new(vec![good(), plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;

    rig.worker().tick()?;

    let saved = memory_store(&rig, "engram");
    assert!(saved.contains("\"topic\":\"Login bug\""));
    assert!(saved.contains("\"pizza-delivery\""));
    assert!(saved.contains("\"job:J1\""));
    assert_eq!(memory_store(&rig, "mnemon"), "");
    assert_eq!(
        write_backs(&rig)?,
        vec![triple("engram", "Login bug", "saved")]
    );
    let artifact = rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::ArtifactRecorded { kind, path, .. } if kind == "memory-writeback" => {
            Some(path)
        }
        _ => None,
    });
    let listing = std::fs::read_to_string(
        rig.config
            .data_dir
            .join(artifact.ok_or_else(|| anyhow!("no memory-writeback artifact"))?),
    )?;
    assert!(listing.contains("Login bug"));
    assert_eq!(rig.job_state()?, JobState::Completed);
    Ok(())
}

#[test]
fn a_second_write_updates_our_own_memory_instead_of_duplicating_it() -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;
    seed_memory(
        &rig,
        "engram",
        "{\"id\":\"7\",\"topic\":\"Login bug\",\"content\":\"old\",\"tags\":[\"pizza-delivery\",\"job:J0\"]}",
    )?;

    rig.worker().tick()?;

    let saved = memory_store(&rig, "engram");
    assert_eq!(saved.lines().count(), 1);
    assert!(saved.contains("hash before compare"));
    assert!(saved.contains("\"job:J1\""));
    assert_eq!(
        write_backs(&rig)?,
        vec![triple("engram", "Login bug", "updated")]
    );
    Ok(())
}

#[test]
fn a_contradiction_with_a_foreign_memory_adds_a_risk_and_skips_the_write() -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;
    let foreign =
        "{\"id\":\"f1\",\"topic\":\"Login bug\",\"content\":\"mine\",\"tags\":[\"owner\"]}";
    seed_memory(&rig, "engram", foreign)?;

    rig.worker().tick()?;

    assert_eq!(memory_store(&rig, "engram").trim(), foreign);
    let risks: Vec<String> = rig
        .timeline()?
        .into_iter()
        .filter_map(|e| match e.payload {
            EventPayload::FeedbackRecorded { kind, text, .. } if kind == "risk" => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(risks.len(), 1);
    assert!(risks[0].contains("f1"));
    assert_eq!(
        write_backs(&rig)?,
        vec![triple("engram", "Login bug", "contradiction")]
    );
    Ok(())
}

#[test]
fn owner_directives_need_a_reply_and_cross_repo_facts_go_to_mnemon_only() -> Result<()> {
    let mut rig = Rig::new(vec![
        good(),
        plan(
            "{\"kind\":\"directive\",\"topic\":\"Style\",\"content\":\"tabs\"}\n{\"kind\":\"cross-repo\",\"topic\":\"Nix\",\"content\":\"flakes everywhere\"}\n",
        ),
    ])?;
    install_memory(&mut rig)?;

    rig.worker().tick()?;

    let saved = memory_store(&rig, "mnemon");
    assert!(saved.contains("Nix"));
    assert!(!saved.contains("Style"));
    assert_eq!(memory_store(&rig, "engram"), "");
    assert!(write_backs(&rig)?.contains(&triple("mnemon", "Style", "skipped-unconfirmed")));
    Ok(())
}

#[test]
fn a_failed_job_is_memorized_too() -> Result<()> {
    let mut rig = Rig::new(vec![
        Script {
            handoff: Some(HANDOFF),
            stdout: ERR_RESULT,
            exit_code: 1,
            ..Script::default()
        },
        plan(
            "{\"kind\":\"failure\",\"topic\":\"Provider 404\",\"content\":\"model missing\"}\n",
        ),
    ])?;
    install_memory(&mut rig)?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Failed);
    assert!(memory_store(&rig, "engram").contains("Provider 404"));
    Ok(())
}

#[test]
fn a_memorize_failure_never_blocks_the_job() -> Result<()> {
    let mut rig = Rig::new(vec![good()])?;
    install_memory(&mut rig)?;

    rig.worker().tick()?;

    assert_eq!(rig.job_state()?, JobState::Completed);
    assert_eq!(memory_store(&rig, "engram"), "");
    assert!(write_backs(&rig)?.is_empty());
    Ok(())
}

#[test]
fn a_cancelled_job_is_not_memorized() -> Result<()> {
    let mut rig = Rig::new(vec![plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;
    let job = JobId::new("J1");
    rig.worker()
        .append(&job, None, EventPayload::JobCancelled {})?;
    rig.worker().append(
        &job,
        None,
        EventPayload::JobFailed {
            reason: "cancelled".into(),
        },
    )?;

    rig.worker().memorize_if_due(&job);

    assert!(rig.prompts()?.is_empty());
    assert_eq!(memory_store(&rig, "engram"), "");
    Ok(())
}

const FAKE_MEMORY_CLI: &str = r#"#!/bin/sh
F="$0.store"
verb=$1
shift
case "$verb" in
search|recall)
  [ -f "$F" ] && grep -F -- "$1" "$F"
  exit 0
  ;;
save|remember|update)
  if [ "$verb" = update ]; then
id=$1
shift
grep -v "\"id\":\"$id\"," "$F" > "$F.tmp"
mv "$F.tmp" "$F"
  else
id=$(( $(cat "$F.n" 2>/dev/null || echo 0) + 1 ))
echo $id > "$F.n"
  fi
  topic=$1
  content=$2
  shift 2
  tags=""
  while [ $# -gt 0 ]; do
if [ "$1" = "--tag" ]; then tags="$tags\"$2\","; fi
shift
  done
  printf '{"id":"%s","topic":"%s","content":"%s","tags":[%s]}\n' "$id" "$topic" "$content" "${tags%,}" >> "$F"
  echo "$id"
  ;;
esac
"#;
