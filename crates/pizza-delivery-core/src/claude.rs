use serde_json::Value;

use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed { reason: String, retryable: bool },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimit {
    pub allowed: bool,
    pub utilization_permille: Option<u32>,
    pub resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRun {
    pub session_id: Option<String>,
    pub usage: Usage,
    pub outcome: Outcome,
    pub slash_commands: Vec<String>,
    pub rate_limit: Option<RateLimit>,
    pub api_error_status: Option<u64>,
    pub result_text: Option<String>,
}

fn epoch_ms(seconds: &Value) -> Option<i64> {
    seconds
        .as_i64()
        .or_else(|| seconds.as_f64().map(|f| f as i64))
        .map(|s| s.saturating_mul(1000))
}

fn rate_limit_of(lines: &[Value]) -> Option<RateLimit> {
    let info = of_type(lines, "rate_limit_event")?.get("rate_limit_info")?;
    let allowed = info
        .get("status")
        .and_then(Value::as_str)
        .map_or(true, |s| s.starts_with("allowed"));
    let hottest = info
        .get("unifiedWindows")
        .and_then(Value::as_object)
        .and_then(|w| {
            w.values()
                .filter_map(|v| Some((v.get("utilization")?.as_f64()?, v.get("resetsAt"))))
                .max_by(|a, b| a.0.total_cmp(&b.0))
        });
    Some(RateLimit {
        allowed,
        utilization_permille: hottest.map(|(u, _)| (u.clamp(0.0, 1.0) * 1000.0).round() as u32),
        resets_at_ms: hottest
            .and_then(|(_, r)| r.and_then(epoch_ms))
            .or_else(|| info.get("resetsAt").and_then(epoch_ms)),
    })
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
        agent: None,
        candidate_position: None,
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
        rate_limit: rate_limit_of(&lines),
        api_error_status: result
            .and_then(|r| r.get("api_error_status"))
            .and_then(Value::as_u64),
        result_text: result.and_then(|r| text(r, "result")),
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

pub const TOOL_SUMMARY_CHARS: usize = 120;
pub const MAX_TOOL_CALLS: usize = 200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    pub summary: String,
}

const SUMMARY_KEYS: [&str; 8] = [
    "command",
    "file_path",
    "path",
    "pattern",
    "url",
    "query",
    "description",
    "skill",
];

fn redact(text: &str) -> String {
    text.split(' ')
        .map(|word| {
            let lower = word.to_ascii_lowercase();
            let secret_key = ["key", "token", "secret", "password", "passwd"]
                .iter()
                .any(|k| lower.split('=').next().is_some_and(|n| n.contains(k)));
            match word.split_once('=') {
                Some((name, _)) if secret_key => format!("{name}=***"),
                _ if word.starts_with("sk-")
                    || word.starts_with("ghp_")
                    || lower.starts_with("bearer") =>
                {
                    "***".to_owned()
                }
                _ => word.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn summarize(input: &Value) -> String {
    let raw = SUMMARY_KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .unwrap_or("");
    let first_line = raw.lines().next().unwrap_or("");
    let clean = redact(first_line);
    if clean.chars().count() > TOOL_SUMMARY_CHARS {
        let cut: String = clean.chars().take(TOOL_SUMMARY_CHARS).collect();
        format!("{cut}…")
    } else {
        clean
    }
}

/// Every `tool_use` block of the stream, in order: name plus a short, redacted summary.
pub fn tool_calls(stdout: &str) -> Vec<ToolCall> {
    parsed(stdout)
        .iter()
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|v| v.get("message")?.get("content")?.as_array())
        .flatten()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
        .filter_map(|b| {
            Some(ToolCall {
                name: b.get("name")?.as_str()?.to_owned(),
                summary: summarize(b.get("input").unwrap_or(&Value::Null)),
            })
        })
        .take(MAX_TOOL_CALLS)
        .collect()
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
    fn the_recorded_rate_limit_event_yields_the_hottest_window_and_its_reset() {
        let run = parse_stream(NORMAL);
        assert_eq!(
            run.rate_limit,
            Some(RateLimit {
                allowed: true,
                utilization_permille: Some(290),
                resets_at_ms: Some(1_794_288_600_000),
            })
        );
    }

    #[test]
    fn a_rejected_rate_limit_event_is_not_allowed() {
        let run = parse_stream(
            "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\",\"resetsAt\":1000}}\n",
        );
        assert_eq!(
            run.rate_limit,
            Some(RateLimit {
                allowed: false,
                utilization_permille: None,
                resets_at_ms: Some(1_000_000),
            })
        );
    }

    #[test]
    fn a_bare_success_result_leaves_unknown_usage_empty() {
        let run = parse_stream("{\"type\":\"result\",\"is_error\":false}\n");
        assert_eq!(run.outcome, Outcome::Succeeded);
        assert_eq!(run.usage, Usage::default());
    }

    #[test]
    fn tool_use_blocks_become_short_redacted_calls() {
        let line = serde_json::json!({"type":"assistant","message":{"content":[
            {"type":"text","text":"hi"},
            {"type":"tool_use","id":"t1","name":"Bash","input":{"command":"API_TOKEN=abc123 curl https://x.io/a"}},
            {"type":"tool_use","id":"t2","name":"Read","input":{"file_path":"/src/lib.rs"}},
            {"type":"tool_use","id":"t3","name":"Write","input":{"file_path":"/a.rs","content":"HUGE"}},
            {"type":"tool_use","id":"t4","name":"Odd","input":{"x":1}}
        ]}});
        let calls = tool_calls(&format!("{line}\nnot json\n"));
        let got: Vec<(&str, &str)> = calls
            .iter()
            .map(|c| (c.name.as_str(), c.summary.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Bash", "API_TOKEN=*** curl https://x.io/a"),
                ("Read", "/src/lib.rs"),
                ("Write", "/a.rs"),
                ("Odd", ""),
            ]
        );
    }

    #[test]
    fn long_summaries_are_truncated_on_a_char_boundary_and_calls_are_capped() {
        let long = "é".repeat(500);
        let block = serde_json::json!({"type":"tool_use","name":"Bash","input":{"command":long}});
        let line = serde_json::json!({"type":"assistant","message":{"content":[block]}});
        let calls = tool_calls(&line.to_string());
        assert_eq!(calls[0].summary.chars().count(), TOOL_SUMMARY_CHARS + 1);
        assert!(calls[0].summary.ends_with('…'));
        let many = (0..MAX_TOOL_CALLS + 5)
            .map(|_| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(tool_calls(&many).len(), MAX_TOOL_CALLS);
    }

    #[test]
    fn recorded_runs_without_tools_have_no_calls() {
        assert!(tool_calls(NORMAL).is_empty());
    }
}
