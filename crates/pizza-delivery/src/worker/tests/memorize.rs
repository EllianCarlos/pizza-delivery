use super::*;

const FAKE_ENGRAM: &str = r#"#!/bin/sh
F="$0.store"
N="$0.n"
die() { echo "$*" >&2; exit 1; }
cmd=$1
[ $# -gt 0 ] && shift
case "$cmd" in
save)
  [ $# -ge 2 ] || die "usage: engram save <title> <content> [--type TYPE] [--project PROJECT] [--scope SCOPE] [--topic TOPIC_KEY]"
  title=$1
  content=$2
  shift 2
  type=manual
  project=""
  key=""
  while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || die "flag $1 needs a value"
    case "$1" in
      --type) type=$2 ;;
      --project) project=$2 ;;
      --scope) ;;
      --topic) key=$2 ;;
      *) die "unknown flag: $1" ;;
    esac
    shift 2
  done
  existing=""
  if [ -n "$key" ] && [ -f "$F" ]; then
    existing=$(awk -F'|' -v k="$key" -v p="$project" '$2==k && $3==p {print $1; exit}' "$F")
  fi
  if [ -n "$existing" ]; then
    id=$existing
    awk -F'|' -v id="$id" '$1!=id' "$F" > "$F.tmp"
    mv "$F.tmp" "$F"
  else
    id=$(( $(cat "$N" 2>/dev/null || echo 0) + 1 ))
    echo $id > "$N"
  fi
  printf '%s|%s|%s|%s|%s|%s\n' "$id" "$key" "$project" "$type" "$title" "$content" >> "$F"
  echo "Memory saved: #$id \"$title\" ($type)"
  ;;
search)
  [ $# -ge 1 ] || die "usage: engram search <query> [--type TYPE] [--project PROJECT] [--scope SCOPE] [--limit N]"
  q=$1
  shift
  project=""
  while [ $# -gt 0 ]; do
    [ $# -ge 2 ] || die "flag $1 needs a value"
    case "$1" in
      --project) project=$2 ;;
      --type|--scope|--limit) ;;
      *) die "unknown flag: $1" ;;
    esac
    shift 2
  done
  out=""
  n=0
  if [ -f "$F" ]; then
    while IFS='|' read -r id key proj type title content; do
      [ -z "$project" ] || [ "$proj" = "$project" ] || continue
      case "$title $content" in *"$q"*) ;; *) continue ;; esac
      n=$((n + 1))
      out="$out[$n] #$id ($type) — $title
    $content
    2026-01-01 00:00:00 | project: $proj | scope: project

"
    done < "$F"
  fi
  if [ "$n" -eq 0 ]; then
    echo "No memories found for: \"$q\""
  else
    printf 'Found %s memories:\n\n%s' "$n" "$out"
  fi
  ;;
*)
  die "Unknown command: $cmd"
  ;;
esac
"#;

const FAKE_MNEMON: &str = r#"#!/bin/sh
F="$0.store"
N="$0.n"
die() { echo "$*" >&2; exit 1; }
nl='
'
cmd=$1
[ $# -gt 0 ] && shift
case "$cmd" in
remember)
  [ $# -ge 1 ] || die "remember needs content"
  content=$1
  shift
  tags=""
  while [ $# -gt 0 ]; do
    case "$1" in
      --no-diff) shift ;;
      --tags) tags=$2; shift 2 ;;
      --cat|--source|--imp|--entities) shift 2 ;;
      *) die "unknown flag: $1" ;;
    esac
  done
  topic=${content%%"$nl"*}
  body=""
  case "$content" in *"$nl"*) body=${content#*"$nl"} ;; esac
  id="m$(( $(cat "$N" 2>/dev/null || echo 0) + 1 ))"
  echo "${id#m}" > "$N"
  printf '%s|%s|%s|%s\n' "$id" "$tags" "$topic" "$body" >> "$F"
  printf '{\n  "action": "added",\n  "id": "%s"\n}\n' "$id"
  ;;
recall)
  [ $# -ge 1 ] || die "recall needs a keyword"
  q=$1
  shift
  while [ $# -gt 0 ]; do
    case "$1" in
      --basic) shift ;;
      --limit|--cat|--intent|--source) shift 2 ;;
      *) die "unknown flag: $1" ;;
    esac
  done
  hits=""
  if [ -f "$F" ]; then
    while IFS='|' read -r id tags topic body; do
      case "$topic $body $tags" in *"$q"*) ;; *) continue ;; esac
      jt=$(echo "$tags" | sed 's/[^,][^,]*/"&"/g')
      hit="{\"insight\":{\"id\":\"$id\",\"content\":\"$topic\\n$body\",\"tags\":[$jt]},\"score\":0.45}"
      if [ -z "$hits" ]; then hits=$hit; else hits="$hits,$hit"; fi
    done < "$F"
  fi
  printf '{"results":[%s],"meta":{"hint":"sparse_results"}}\n' "$hits"
  ;;
forget)
  [ $# -eq 1 ] || die "forget needs an id"
  grep -q "^$1|" "$F" 2>/dev/null || die "insight $1 not found"
  grep -v "^$1|" "$F" > "$F.tmp"
  mv "$F.tmp" "$F"
  echo '{"action":"forgotten"}'
  ;;
*)
  die "Unknown command: $cmd"
  ;;
esac
"#;

fn install_memory(rig: &mut Rig) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let engram = rig.config.data_dir.join("engram");
    let mnemon = rig.config.data_dir.join("mnemon");
    for (path, script) in [(&engram, FAKE_ENGRAM), (&mnemon, FAKE_MNEMON)] {
        std::fs::write(path, script)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    rig.config.memory = Some(MemorySetup { engram, mnemon });
    Ok(())
}

fn memory_store(rig: &Rig, name: &str) -> String {
    std::fs::read_to_string(rig.config.data_dir.join(format!("{name}.store"))).unwrap_or_default()
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

const NIX_PLAN: &str =
    "{\"kind\":\"cross-repo\",\"topic\":\"Nix\",\"content\":\"flakes everywhere\"}\n";

#[test]
fn a_completed_job_saves_project_facts_to_engram_and_reconciles_them_by_job_marker() -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;

    rig.worker().tick()?;

    assert_eq!(
        memory_store(&rig, "engram"),
        "1|pizza-delivery/login-bug|my repo|bugfix|Login bug|[pizza-delivery job:J1] hash before compare\n"
    );
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
fn a_second_write_updates_our_own_engram_memory_by_topic_key_instead_of_duplicating_it(
) -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(FIX_PLAN)])?;
    install_memory(&mut rig)?;
    seed_memory(
        &rig,
        "engram",
        "7|pizza-delivery/login-bug|my repo|bugfix|Login bug|[pizza-delivery job:J0] old",
    )?;

    rig.worker().tick()?;

    assert_eq!(
        memory_store(&rig, "engram"),
        "7|pizza-delivery/login-bug|my repo|bugfix|Login bug|[pizza-delivery job:J1] hash before compare\n"
    );
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
    let foreign = "7||my repo|bugfix|Login bug|mine";
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
    assert!(risks[0].contains("id 7"));
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

    assert_eq!(
        memory_store(&rig, "mnemon"),
        "m1|pizza-delivery,job:J1|Nix|flakes everywhere\n"
    );
    assert_eq!(memory_store(&rig, "engram"), "");
    assert!(write_backs(&rig)?.contains(&triple("mnemon", "Style", "skipped-unconfirmed")));
    assert!(write_backs(&rig)?.contains(&triple("mnemon", "Nix", "saved")));
    Ok(())
}

#[test]
fn a_second_cross_repo_write_replaces_our_own_mnemon_memory_without_a_duplicate() -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(NIX_PLAN)])?;
    install_memory(&mut rig)?;
    seed_memory(&rig, "mnemon", "m9|pizza-delivery,job:J0|Nix|flakes")?;

    rig.worker().tick()?;

    assert_eq!(
        memory_store(&rig, "mnemon"),
        "m1|pizza-delivery,job:J1|Nix|flakes everywhere\n"
    );
    assert_eq!(write_backs(&rig)?, vec![triple("mnemon", "Nix", "updated")]);
    Ok(())
}

#[test]
fn a_foreign_mnemon_memory_is_never_replaced() -> Result<()> {
    let mut rig = Rig::new(vec![good(), plan(NIX_PLAN)])?;
    install_memory(&mut rig)?;
    seed_memory(&rig, "mnemon", "m9|owner|Nix|flakes")?;

    rig.worker().tick()?;

    assert_eq!(memory_store(&rig, "mnemon"), "m9|owner|Nix|flakes\n");
    assert_eq!(
        write_backs(&rig)?,
        vec![triple("mnemon", "Nix", "contradiction")]
    );
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
        plan("{\"kind\":\"failure\",\"topic\":\"Provider 404\",\"content\":\"model missing\"}\n"),
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
