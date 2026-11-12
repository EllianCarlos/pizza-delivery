//! `pizza-delivery routing check`: resolve the routing file the way the
//! daemon would and flag what cannot work, without needing the daemon.

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

use pizza_delivery_core::routing::{
    check, model_known, model_list_args, parse_model_list, AdapterKind, CheckReport, RoutingConfig,
};

use crate::config::Config;

const LIST_TIMEOUT: Duration = Duration::from_secs(10);

fn adapter_name(kind: AdapterKind) -> &'static str {
    match kind {
        AdapterKind::Claude => "claude",
        AdapterKind::Opencode => "opencode",
        AdapterKind::Hermes => "hermes",
        AdapterKind::Agy => "agy",
    }
}

fn candidate_label(agent: &str, model: Option<&str>) -> String {
    model.map_or_else(|| agent.to_owned(), |m| format!("{agent}:{m}"))
}

pub fn render(config: &Config, report: &CheckReport) -> String {
    let source = config.routing_file.as_ref().map_or_else(
        || "built-in defaults".to_owned(),
        |f| {
            if f.path.exists() {
                f.path.display().to_string()
            } else {
                format!("{} (absent, built-in defaults)", f.path.display())
            }
        },
    );
    let mut out = format!(
        "routing file: {source}\nquota threshold: {}% (an agent at or above it is skipped until its window resets)\ndefault backoff: {}s\n\n",
        f64::from(report.threshold_permille) / 10.0,
        report.default_backoff_ms / 1000
    );
    for stage in &report.stages {
        let chain = stage
            .chain
            .iter()
            .map(|c| {
                let label = candidate_label(&c.candidate.agent, c.candidate.model.as_deref());
                if c.usable {
                    label
                } else {
                    format!("{label} (unusable: lacks required capability)")
                }
            })
            .collect::<Vec<_>>()
            .join(" -> ");
        let rule = stage
            .rule
            .map_or_else(|| "no rule".to_owned(), |r| format!("rules[{r}]"));
        out.push_str(&format!("{:<12} {:<9} {chain}\n", stage.stage, rule));
        if let Some(problem) = &stage.problem {
            out.push_str(&format!("  error: {}: {problem}\n", stage.stage));
        }
    }
    out
}

fn list_models(command: &str, args: &[&str]) -> Result<Vec<String>, String> {
    let (command, args) = (
        command.to_owned(),
        args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>(),
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(Command::new(&command).args(&args).output());
    });
    match rx.recv_timeout(LIST_TIMEOUT) {
        Err(_) => Err("timed out".to_owned()),
        Ok(Err(e)) => Err(e.to_string()),
        Ok(Ok(out)) if out.status.success() => {
            Ok(parse_model_list(&String::from_utf8_lossy(&out.stdout)))
        }
        Ok(Ok(out)) => Err(format!(
            "exit {}",
            out.status
                .code()
                .map_or("signal".to_owned(), |c| c.to_string())
        )),
    }
}

/// Best-effort model warnings; never an error.
pub fn model_warnings(routing: &RoutingConfig, report: &CheckReport) -> Vec<String> {
    let used: BTreeMap<&str, BTreeSet<&str>> = report
        .stages
        .iter()
        .flat_map(|s| &s.chain)
        .filter_map(|c| {
            c.candidate
                .model
                .as_deref()
                .map(|m| (c.candidate.agent.as_str(), m))
        })
        .fold(BTreeMap::new(), |mut acc, (agent, model)| {
            acc.entry(agent).or_default().insert(model);
            acc
        });
    used.into_iter()
        .filter_map(|(agent, models)| {
            let def = routing.agents.get(agent)?;
            let args = model_list_args(def.adapter)?;
            let adapter = adapter_name(def.adapter);
            Some(match list_models(&def.command, args) {
                Err(reason) => vec![format!(
                    "warning: agent '{agent}': could not list models via `{adapter} {}` ({reason}); model names not verified",
                    args.join(" ")
                )],
                Ok(listed) => models
                    .into_iter()
                    .filter(|m| !model_known(&listed, m))
                    .map(|m| {
                        format!(
                            "warning: agent '{agent}' model '{m}' is not in `{adapter} {}`",
                            args.join(" ")
                        )
                    })
                    .collect(),
            })
        })
        .flatten()
        .collect()
}

/// Returns the text to print and whether the routing is usable.
pub fn run(config: &Config) -> (String, bool) {
    let report = check(&config.routing);
    let mut out = render(config, &report);
    let warnings = model_warnings(&config.routing, &report);
    if !warnings.is_empty() {
        out.push('\n');
        warnings.iter().for_each(|w| {
            out.push_str(w);
            out.push('\n');
        });
    }
    (out, report.is_ok())
}
