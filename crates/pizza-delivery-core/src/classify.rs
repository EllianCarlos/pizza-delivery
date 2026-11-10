use serde_json::Value;

use crate::claude::{parse_stream, AgentRun, Outcome};
use crate::quota::QuotaStatus;
use crate::routing::AdapterKind;
use crate::usage::Usage;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Transient(String),
    QuotaExhausted {
        resets_at_ms: Option<i64>,
        reason: String,
    },
    ProviderError(String),
    Timeout,
    Crash(String),
    Config(String),
}

impl Failure {
    pub fn reason(&self) -> String {
        match self {
            Failure::Transient(r)
            | Failure::ProviderError(r)
            | Failure::Crash(r)
            | Failure::Config(r) => r.clone(),
            Failure::QuotaExhausted { reason, .. } => format!("quota exhausted: {reason}"),
            Failure::Timeout => "agent timed out".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RawRun<'a> {
    pub exit_code: Option<i32>,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub timed_out: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub status: QuotaStatus,
    pub utilization_permille: Option<u32>,
    pub resets_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classified {
    pub usage: Usage,
    pub session_id: Option<String>,
    pub observation: Option<Observation>,
    pub failure: Option<Failure>,
}

const QUOTA_PHRASES: [&str; 9] = [
    "rate limit",
    "rate_limit",
    "ratelimit",
    "usage limit",
    "quota",
    "too many requests",
    "resource_exhausted",
    "limit reached",
    "limit exceeded",
];

fn has_status_429(text: &str) -> bool {
    text.match_indices("429").any(|(i, _)| {
        let before = text[..i].chars().next_back();
        let after = text[i + 3..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

pub fn mentions_quota(text: &str) -> bool {
    let lower = text.to_lowercase();
    QUOTA_PHRASES.iter().any(|p| lower.contains(p)) || has_status_429(&lower)
}

fn overloaded(status: u64) -> bool {
    matches!(status, 500 | 502 | 503 | 504 | 529)
}

fn claude_failure(run: &AgentRun, exit_code: Option<i32>, raw_text: &str) -> Option<Failure> {
    let rejected = run.rate_limit.as_ref().filter(|r| !r.allowed);
    let quota = |reason: String| Failure::QuotaExhausted {
        resets_at_ms: run.rate_limit.as_ref().and_then(|r| r.resets_at_ms),
        reason,
    };
    match &run.outcome {
        Outcome::Succeeded => None,
        Outcome::Failed {
            reason,
            retryable: true,
        } => Some(if rejected.is_some() || mentions_quota(raw_text) {
            quota(reason.clone())
        } else {
            Failure::Transient(match exit_code {
                Some(code) => format!("{reason} (exit {code})"),
                None => reason.clone(),
            })
        }),
        Outcome::Failed { reason, .. } => {
            let status = run.api_error_status;
            let text = run.result_text.as_deref().unwrap_or_default();
            Some(
                if rejected.is_some()
                    || status == Some(429)
                    || mentions_quota(text)
                    || mentions_quota(reason)
                {
                    quota(reason.clone())
                } else if status.is_some_and(overloaded) {
                    Failure::Transient(reason.clone())
                } else {
                    Failure::ProviderError(reason.clone())
                },
            )
        }
    }
}

fn claude(run: &RawRun<'_>, required_skill: Option<&str>) -> Classified {
    let parsed = parse_stream(run.stdout);
    let observation = parsed.rate_limit.as_ref().map(|r| Observation {
        status: if r.allowed {
            QuotaStatus::Allowed
        } else {
            QuotaStatus::Exhausted
        },
        utilization_permille: r.utilization_permille,
        resets_at_ms: r.resets_at_ms,
    });
    let missing_skill = required_skill.filter(|skill| {
        !parsed.slash_commands.is_empty() && !parsed.slash_commands.iter().any(|s| s == skill)
    });
    let failure = if let Some(skill) = missing_skill {
        Some(Failure::Config(format!("skill '{skill}' is not installed")))
    } else if run.timed_out {
        Some(Failure::Timeout)
    } else {
        claude_failure(
            &parsed,
            run.exit_code,
            &format!("{}\n{}", run.stdout, run.stderr),
        )
    };
    Classified {
        usage: parsed.usage,
        session_id: parsed.session_id,
        observation,
        failure,
    }
}

fn has_error_event(stdout: &str) -> bool {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .any(|v| v.get("type").and_then(Value::as_str) == Some("error"))
}

fn is_credential_failure(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("no usable credentials")
        || lower.contains("credentials or agent init failed")
        || lower.contains("environment variable is not set")
}

fn reactive(kind: AdapterKind, run: &RawRun<'_>) -> Option<Failure> {
    if run.timed_out {
        return Some(Failure::Timeout);
    }
    let combined = format!("{}\n{}", run.stdout, run.stderr);
    let first_line = |text: &str| {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>()
    };
    match run.exit_code {
        Some(0) => None,
        None => Some(Failure::Crash("agent terminated by a signal".to_owned())),
        Some(code) => {
            let detail = if run.stderr.trim().is_empty() {
                first_line(run.stdout)
            } else {
                first_line(run.stderr)
            };
            let reason = format!("exit {code}: {detail}");
            if matches!(kind, AdapterKind::Hermes | AdapterKind::Agy)
                && is_credential_failure(&combined)
            {
                return Some(Failure::Config(reason));
            }
            let quota_classifier_enabled = !matches!(kind, AdapterKind::Hermes);
            if quota_classifier_enabled && mentions_quota(&combined) {
                return Some(Failure::QuotaExhausted {
                    resets_at_ms: None,
                    reason,
                });
            }
            let known_provider_failure = match kind {
                AdapterKind::Opencode => has_error_event(run.stdout),
                AdapterKind::Hermes | AdapterKind::Agy => true,
                AdapterKind::Claude => false,
            };
            Some(if known_provider_failure {
                Failure::ProviderError(reason)
            } else {
                Failure::Crash(reason)
            })
        }
    }
}

pub fn classify(kind: AdapterKind, run: &RawRun<'_>, required_skill: Option<&str>) -> Classified {
    match kind {
        AdapterKind::Claude => claude(run, required_skill),
        other => Classified {
            usage: Usage::default(),
            session_id: None,
            observation: None,
            failure: reactive(other, run),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($cli:literal, $case:literal) => {
            fixture!($cli, $case, "jsonl")
        };
        ($cli:literal, $case:literal, $ext:literal) => {
            (
                include_str!(concat!(
                    "../../../docs/fixtures/",
                    $cli,
                    "/",
                    $case,
                    ".stdout.",
                    $ext
                )),
                include_str!(concat!(
                    "../../../docs/fixtures/",
                    $cli,
                    "/",
                    $case,
                    ".stderr.txt"
                )),
                include_str!(concat!(
                    "../../../docs/fixtures/",
                    $cli,
                    "/",
                    $case,
                    ".exitcode"
                )),
            )
        };
    }

    fn run_of(fixture: (&str, &str, &str)) -> (String, String, Option<i32>) {
        (
            fixture.0.to_owned(),
            fixture.1.to_owned(),
            fixture.2.trim().parse().ok(),
        )
    }

    fn classify_fixture(
        kind: AdapterKind,
        fixture: (&str, &str, &str),
        skill: Option<&str>,
    ) -> Classified {
        let (stdout, stderr, exit_code) = run_of(fixture);
        classify(
            kind,
            &RawRun {
                exit_code,
                stdout: &stdout,
                stderr: &stderr,
                timed_out: false,
            },
            skill,
        )
    }

    fn raw(exit_code: i32, stdout: &str, stderr: &str) -> Classified {
        classify(
            AdapterKind::Opencode,
            &RawRun {
                exit_code: Some(exit_code),
                stdout,
                stderr,
                timed_out: false,
            },
            None,
        )
    }

    #[test]
    fn a_recorded_normal_claude_run_has_no_failure_and_reports_a_healthy_window() {
        let c = classify_fixture(AdapterKind::Claude, fixture!("claude", "normal"), None);
        assert_eq!(c.failure, None);
        assert_eq!(
            c.observation
                .as_ref()
                .map(|o| (o.status, o.utilization_permille)),
            Some((QuotaStatus::Allowed, Some(290)))
        );
    }

    #[test]
    fn claude_unknown_model_and_budget_cap_are_provider_errors_not_quota() {
        let model = classify_fixture(
            AdapterKind::Claude,
            fixture!("claude", "error-bad-model"),
            None,
        );
        let budget = classify_fixture(
            AdapterKind::Claude,
            fixture!("claude", "error-budget-cap"),
            None,
        );
        assert!(matches!(model.failure, Some(Failure::ProviderError(_))));
        assert!(matches!(budget.failure, Some(Failure::ProviderError(_))));
    }

    #[test]
    fn claude_max_turns_is_a_provider_error_not_quota() {
        let c = classify_fixture(
            AdapterKind::Claude,
            fixture!("claude", "normal-maxturns1-stophook"),
            None,
        );
        assert!(matches!(c.failure, Some(Failure::ProviderError(_))));
    }

    #[test]
    fn a_missing_skill_is_a_configuration_error_even_though_claude_exits_zero() {
        let missing = classify_fixture(
            AdapterKind::Claude,
            fixture!("claude", "skill-unknown"),
            Some("nonexistent-skill-xyz"),
        );
        let present = classify_fixture(
            AdapterKind::Claude,
            fixture!("claude", "skill-to-spec"),
            Some("to-spec"),
        );
        assert!(matches!(missing.failure, Some(Failure::Config(_))));
        assert_eq!(present.failure, None);
    }

    #[test]
    fn opencode_fixtures_classify_as_success_and_provider_error() {
        let ok = classify_fixture(AdapterKind::Opencode, fixture!("opencode", "normal"), None);
        let bad = classify_fixture(
            AdapterKind::Opencode,
            fixture!("opencode", "error-bad-model"),
            None,
        );
        assert_eq!(ok.failure, None);
        assert!(matches!(bad.failure, Some(Failure::ProviderError(_))));
    }

    #[test]
    fn missing_credentials_for_hermes_and_agy_are_terminal_configuration_errors() {
        let hermes = classify_fixture(AdapterKind::Hermes, fixture!("hermes", "normal"), None);
        let agy = classify_fixture(AdapterKind::Agy, fixture!("agy", "normal", "txt"), None);
        assert!(matches!(hermes.failure, Some(Failure::Config(_))));
        assert!(matches!(agy.failure, Some(Failure::Config(_))));
    }

    #[test]
    fn a_rejected_claude_rate_limit_event_with_an_error_result_is_quota_with_its_reset() {
        let stdout = concat!(
            "{\"type\":\"rate_limit_event\",\"rate_limit_info\":{\"status\":\"rejected\",\"resetsAt\":2000}}\n",
            "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429,\"result\":\"limit\"}\n"
        );
        let c = classify(
            AdapterKind::Claude,
            &RawRun {
                exit_code: Some(1),
                stdout,
                stderr: "",
                timed_out: false,
            },
            None,
        );
        assert!(matches!(
            c.failure,
            Some(Failure::QuotaExhausted {
                resets_at_ms: Some(2_000_000),
                ..
            })
        ));
        assert_eq!(
            c.observation.map(|o| o.status),
            Some(QuotaStatus::Exhausted)
        );
    }

    #[test]
    fn claude_429_without_a_rate_limit_event_is_quota_with_an_unknown_reset() {
        let stdout = "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":429}\n";
        let c = classify(
            AdapterKind::Claude,
            &RawRun {
                exit_code: Some(1),
                stdout,
                stderr: "",
                timed_out: false,
            },
            None,
        );
        assert!(matches!(
            c.failure,
            Some(Failure::QuotaExhausted {
                resets_at_ms: None,
                ..
            })
        ));
    }

    #[test]
    fn claude_overload_is_transient_and_a_stream_without_a_result_is_transient() {
        let overloaded = classify(
            AdapterKind::Claude,
            &RawRun {
                exit_code: Some(1),
                stdout: "{\"type\":\"result\",\"is_error\":true,\"terminal_reason\":\"api_error\",\"api_error_status\":529}\n",
                stderr: "",
                timed_out: false,
            },
            None,
        );
        let truncated = classify(
            AdapterKind::Claude,
            &RawRun {
                exit_code: Some(1),
                stdout: "garbage",
                stderr: "",
                timed_out: false,
            },
            None,
        );
        assert!(matches!(overloaded.failure, Some(Failure::Transient(_))));
        assert!(matches!(truncated.failure, Some(Failure::Transient(_))));
    }

    #[test]
    fn a_timeout_wins_over_whatever_the_dead_process_printed() {
        let c = classify(
            AdapterKind::Claude,
            &RawRun {
                exit_code: None,
                stdout: "",
                stderr: "",
                timed_out: true,
            },
            None,
        );
        assert_eq!(c.failure, Some(Failure::Timeout));
    }

    #[test]
    fn reactive_adapters_read_quota_from_text_on_either_stream() {
        let stderr_only = raw(1, "", "Error: 429 Too Many Requests");
        let json_error = raw(
            1,
            "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit\",\"message\":\"Quota exceeded\"}}\n",
            "",
        );
        assert!(matches!(
            stderr_only.failure,
            Some(Failure::QuotaExhausted {
                resets_at_ms: None,
                ..
            })
        ));
        assert!(matches!(
            json_error.failure,
            Some(Failure::QuotaExhausted { .. })
        ));
    }

    #[test]
    fn a_number_containing_429_is_not_a_quota_signal() {
        let c = raw(
            1,
            "{\"type\":\"error\",\"error\":{\"message\":\"model gpt-14290 missing\"}}\n",
            "",
        );
        assert!(matches!(c.failure, Some(Failure::ProviderError(_))));
    }

    #[test]
    fn hermes_quota_classification_stays_disabled_until_a_real_fixture_exists() {
        let c = classify(
            AdapterKind::Hermes,
            &RawRun {
                exit_code: Some(1),
                stdout: "",
                stderr: "rate limit exceeded",
                timed_out: false,
            },
            None,
        );
        assert!(matches!(c.failure, Some(Failure::ProviderError(_))));
    }

    #[test]
    fn a_non_zero_exit_with_no_recognised_error_is_a_crash_and_a_signal_death_too() {
        let silent = raw(137, "", "");
        let signalled = classify(
            AdapterKind::Opencode,
            &RawRun {
                exit_code: None,
                stdout: "",
                stderr: "",
                timed_out: false,
            },
            None,
        );
        assert!(matches!(silent.failure, Some(Failure::Crash(_))));
        assert!(matches!(signalled.failure, Some(Failure::Crash(_))));
    }
}
