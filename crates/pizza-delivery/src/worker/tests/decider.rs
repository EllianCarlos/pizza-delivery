use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;

use super::*;
use crate::config::ShadowConfig;

/// A one-shot stand-in for Ollama's /api/chat that picks `model` for `stage`.
fn fake_ollama(stage: &str, model: &str) -> Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}", listener.local_addr()?);
    let picks = format!(
        r#"{{"stages":[{{"stage":"{stage}","model":"{model}","reason":"small change"}}]}}"#
    );
    let body = serde_json::json!({ "message": { "content": picks } }).to_string();
    std::thread::spawn(move || {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        let mut reader = BufReader::new(stream);
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap_or(0);
            }
        }
        let mut request = vec![0; length];
        let _ = reader.read_exact(&mut request);
        let reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = reader.get_mut().write_all(reply.as_bytes());
    });
    Ok(url)
}

fn ran_model(rig: &Rig) -> Result<Option<String>> {
    Ok(rig.timeline()?.into_iter().find_map(|e| match e.payload {
        EventPayload::AgentFinished { usage } => usage.model,
        _ => None,
    }))
}

fn stored_decision(rig: &Rig) -> Result<serde_json::Value> {
    let path = rig
        .timeline()?
        .into_iter()
        .find_map(|e| match e.payload {
            EventPayload::ArtifactRecorded { kind, path, .. } if kind == "model-decider" => {
                Some(path)
            }
            _ => None,
        })
        .ok_or_else(|| anyhow!("no model-decider artifact"))?;
    Ok(serde_json::from_slice(&std::fs::read(
        rig.config.data_dir.join(path),
    )?)?)
}

fn rig_with_decider(live_percent: u8) -> Result<Rig> {
    let mut rig = Rig::new(vec![good()])?;
    rig.config.shadow = Some(ShadowConfig {
        url: fake_ollama("implement", "haiku")?,
        model: "qwen-test".into(),
        sop: None,
        live_percent,
    });
    plan_of(&rig, &["implement"])?;
    Ok(rig)
}

#[test]
fn a_live_arm_job_runs_the_model_the_decider_picked() -> Result<()> {
    let rig = rig_with_decider(100)?;

    rig.worker().tick()?;

    assert_eq!(ran_model(&rig)?.as_deref(), Some("haiku"));
    let decision = stored_decision(&rig)?;
    assert_eq!(decision["arm"], "live");
    assert_eq!(decision["decider"], "ollama:qwen-test");
    Ok(())
}

#[test]
fn a_control_arm_job_records_the_picks_but_runs_the_default() -> Result<()> {
    let rig = rig_with_decider(0)?;

    rig.worker().tick()?;

    assert_eq!(ran_model(&rig)?.as_deref(), Some("sonnet"));
    let decision = stored_decision(&rig)?;
    assert_eq!(decision["arm"], "shadow");
    assert_eq!(decision["picks"][0]["model"], "haiku");
    Ok(())
}
