use serde_json::Value;

use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed { reason: String, retryable: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub session_id: Option<String>,
    pub usage: Usage,
    pub outcome: Outcome,
    pub slash_commands: Vec<String>,
}

fn parsed(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

fn of_type<'a>(lines: &'a [Value], ty: &str) -> Option<&'a Value> {
    lines
        .iter()
        .rev()
        .find(|v| v.get("type").and_then(Value::as_str) == Some(ty))
}

fn init(lines: &[Value]) -> Option<&Value> {
    lines.iter().find(|v| {
        v.get("type").and_then(Value::as_str) == Some("system")
            && v.get("subtype").and_then(Value::as_str) == Some("init")
    })
}

fn text(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn sum(values: &[Option<u64>]) -> Option<u64> {
    values
        .iter()
        .flatten()
        .copied()
        .fold(None, |acc, n| Some(acc.unwrap_or(0) + n))
}

fn usage_of(init: Option<&Value>, result: Option<&Value>) -> Usage {
    let counters = result.and_then(|r| r.get("usage"));
    let count = |key: &str| counters.and_then(|u| u.get(key)).and_then(Value::as_u64);
    Usage {
        model: init.and_then(|i| text(i, "model")),
        input_tokens: sum(&[
            count("input_tokens"),
            count("cache_creation_input_tokens"),
            count("cache_read_input_tokens"),
        ]),
        output_tokens: count("output_tokens"),
        cost_microusd: result
            .and_then(|r| r.get("total_cost_usd"))
            .and_then(Value::as_f64)
            .map(|usd| (usd * 1_000_000.0).round() as u64),
        wall_ms: None,
    }
}

fn outcome_of(result: Option<&Value>) -> Outcome {
    match result {
        None => Outcome::Failed {
            reason: "agent stream ended without a result".to_owned(),
            retryable: true,
        },
        Some(r) if r.get("is_error").and_then(Value::as_bool) == Some(true) => {
            let reason = text(r, "terminal_reason").unwrap_or_else(|| "error".to_owned());
            let status = r
                .get("api_error_status")
                .and_then(Value::as_u64)
                .map(|s| format!(" (status {s})"))
                .unwrap_or_default();
            Outcome::Failed {
                reason: format!("{reason}{status}"),
                retryable: false,
            }
        }
        Some(_) => Outcome::Succeeded,
    }
}

pub fn parse_stream(stdout: &str) -> AgentRun {
    let lines = parsed(stdout);
    let init = init(&lines);
    let result = of_type(&lines, "result");
    AgentRun {
        session_id: result
            .and_then(|r| text(r, "session_id"))
            .or_else(|| init.and_then(|i| text(i, "session_id"))),
        usage: usage_of(init, result),
        outcome: outcome_of(result),
        slash_commands: init
            .and_then(|i| i.get("slash_commands"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: &str = include_str!("../../../docs/fixtures/claude/normal.stdout.jsonl");
    const BAD_MODEL: &str =
        include_str!("../../../docs/fixtures/claude/error-bad-model.stdout.jsonl");

    #[test]
    fn the_recorded_normal_run_succeeds_with_normalized_usage() {
        let run = parse_stream(NORMAL);
        assert_eq!(run.outcome, Outcome::Succeeded);
        assert_eq!(
            run.session_id.as_deref(),
            Some("97fa0650-5bea-4450-8e4c-06423adbcf2b")
        );
        assert_eq!(run.usage.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(run.usage.input_tokens, Some(101_196));
        assert_eq!(run.usage.output_tokens, Some(8));
        assert_eq!(run.usage.cost_microusd, Some(301_268));
    }

    #[test]
    fn an_unknown_model_fails_even_though_the_subtype_says_success() {
        let run = parse_stream(BAD_MODEL);
        assert_eq!(
            run.outcome,
            Outcome::Failed {
                reason: "api_error (status 404)".to_owned(),
                retryable: false
            }
        );
    }

    #[test]
    fn a_stream_without_a_result_is_a_retryable_failure_with_unknown_usage() {
        let run = parse_stream("not json\n{\"type\":\"assistant\"}\n");
        assert!(matches!(
            run.outcome,
            Outcome::Failed {
                retryable: true,
                ..
            }
        ));
        assert_eq!(run.usage, Usage::default());
    }

    #[test]
    fn a_bare_success_result_leaves_unknown_usage_empty() {
        let run = parse_stream("{\"type\":\"result\",\"is_error\":false}\n");
        assert_eq!(run.outcome, Outcome::Succeeded);
        assert_eq!(run.usage, Usage::default());
    }
}
