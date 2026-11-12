mod harness;
mod mcp_mock;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use harness::{Harness, TestResult, VALID_HANDOFF_AGENT};

const FAKE_PRELUDE: &str = r#"echo "$$" >> "$dir/server.pids""#;

const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
const ACTIVATE_DOCS: &str = r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"mcp_activate","arguments":{"name":"docs"}}}"#;
const CALL_DOCS: &str = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"docs__echo","arguments":{}}}"#;
const ACTIVATE_BROWSER: &str = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"mcp_activate","arguments":{"name":"browser"}}}"#;

fn alive(pid: &str) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => false,
    }
}

fn agent(session: &str) -> String {
    let body = VALID_HANDOFF_AGENT.trim_start_matches("#!/bin/sh\n");
    format!(
        r#"#!/bin/sh
dir=$(dirname "$PIZZA_DELIVERY_HANDOFF")
STAGE=$(basename "$dir" | sed 's/^[0-9]*-//')
if [ "$STAGE" = grill ]; then
  case "$*" in *--mcp-config*) ;; *) exit 9;; esac
  cfg="$dir/mcp.json"
  cmd=$(sed 's/.*"command":"\([^"]*\)".*/\1/' "$cfg")
  IFS='
'
  set -f
  set -- $(sed 's/.*"args":\[\(.*\)\],"command".*/\1/' "$cfg" | tr -d '"' | tr ',' '\n')
  unset IFS
{session}
fi
{body}"#
    )
}

fn harness(session: &str) -> TestResult<(Harness, String)> {
    let h = Harness::with_agent(&agent(session))?;
    let data = h.data_dir();
    fs::create_dir_all(data.join("secrets"))?;
    let server = data.join("server.sh");
    fs::write(&server, mcp_mock::fake_server(FAKE_PRELUDE))?;
    fs::set_permissions(&server, fs::Permissions::from_mode(0o755))?;
    fs::write(
        data.join("mcp-catalog.toml"),
        format!(
            "[[server]]\nname = \"docs\"\ncommand = [\"{s}\"]\nauto = true\n\n[[server]]\nname = \"browser\"\ncommand = [\"{s}\"]\n",
            s = server.display()
        ),
    )?;
    let pids = data.join("server.pids").to_string_lossy().into_owned();
    Ok((h, pids))
}

#[test]
fn a_stage_ledgers_mcp_requests_denials_feedback_calls_and_activations() -> TestResult {
    let session = format!(
        "printf '%s\\n' '{INIT}' '{ACTIVATE_DOCS}' '{CALL_DOCS}' '{ACTIVATE_BROWSER}' | \"$cmd\" \"$@\" > /dev/null"
    );
    let (h, _) = harness(&session)?;
    let id = h.submit_in_repo("use docs")?;
    let job = h.wait_for_end(&id)?;
    assert_eq!(job["state"], "completed");

    let rows: Vec<_> = h.ledger()?.into_iter().filter(|r| r.job_id == id).collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r.kind.as_str()).collect();
    for kind in [
        "mcp-requested",
        "mcp-activated",
        "mcp-denied",
        "mcp-calls",
        "mcp-deactivated",
    ] {
        assert!(kinds.contains(&kind), "missing {kind} in {kinds:?}");
    }
    let calls = rows.iter().find(|r| r.kind == "mcp-calls");
    assert_eq!(calls.map(|r| r.payload["count"].clone()), Some(1.into()));
    let feedback = rows.iter().find(|r| {
        r.kind == "feedback-recorded"
            && r.payload["text"]
                .as_str()
                .is_some_and(|t| t.contains("'browser'"))
    });
    assert_eq!(feedback.map(|r| r.actor.as_str()), Some("agent"));
    let audit = rows.iter().find(|r| {
        r.kind == "sandbox-audit" && r.payload["mcp_activations"] != serde_json::json!([])
    });
    assert_eq!(
        audit.map(|r| r.payload["mcp_activations"].clone()),
        Some(serde_json::json!(["docs"]))
    );
    Ok(())
}

#[test]
fn servers_do_not_outlive_a_stage_whose_agent_exits_without_cleanup() -> TestResult {
    let session = format!(
        "(printf '%s\\n' '{INIT}' '{ACTIVATE_DOCS}'; sleep 300) | \"$cmd\" \"$@\" > /dev/null &\nsleep 1"
    );
    let (h, pids) = harness(&session)?;
    let id = h.submit_in_repo("crash")?;
    h.wait_for_end(&id)?;

    let pids: Vec<String> = fs::read_to_string(&pids)?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    assert!(!pids.is_empty());
    let deadline = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|p| alive(p)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    for pid in &pids {
        assert!(!alive(pid), "server {pid} survived the stage");
    }
    Ok(())
}
