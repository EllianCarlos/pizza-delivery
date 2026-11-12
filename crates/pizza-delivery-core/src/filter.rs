//! Pure filter and search over ledger events (see GLOSSARY: Filter, Search, Inbox item).

use serde::{Deserialize, Serialize};

use crate::event::{Event, EventPayload};
use crate::id::{short_id, JobId};

/// Constraints shared by every list. Different properties are ANDed, repeated
/// values of one property are ORed. `jobs` holds full job ids once resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    #[serde(default)]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub actors: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFilter {
    #[serde(default)]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Substrings of the artifact path (ORed).
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxFilter {
    #[serde(default)]
    pub jobs: Vec<String>,
    #[serde(default)]
    pub stages: Vec<String>,
    #[serde(default)]
    pub kinds: Vec<String>,
    #[serde(default)]
    pub severities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_confidence: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub event_id: String,
    pub job_id: JobId,
    pub short_id: String,
    pub stage_id: Option<String>,
    pub ts_ms: i64,
    pub kind: String,
    pub severity: String,
    pub text: String,
    pub confidence: Option<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactItem {
    pub event_id: String,
    pub job_id: JobId,
    pub short_id: String,
    pub stage_id: Option<String>,
    pub ts_ms: i64,
    pub kind: String,
    pub path: String,
    pub sha256: String,
}

fn any_match(wanted: &[String], value: &str) -> bool {
    wanted.is_empty() || wanted.iter().any(|w| w == value)
}

fn stage_match(wanted: &[String], stage: Option<&str>) -> bool {
    wanted.is_empty() || stage.is_some_and(|s| wanted.iter().any(|w| w == s))
}

fn in_range(ts: i64, since: Option<i64>, until: Option<i64>) -> bool {
    since.map_or(true, |s| ts >= s) && until.map_or(true, |u| ts <= u)
}

/// `Err` when the range is inverted.
pub fn check_range(since: Option<i64>, until: Option<i64>) -> Result<(), String> {
    match (since, until) {
        (Some(s), Some(u)) if s > u => Err("--since must not be later than --until".to_string()),
        _ => Ok(()),
    }
}

fn contains_ci(haystack: &str, needle_lower: &str) -> bool {
    haystack.to_lowercase().contains(needle_lower)
}

fn normalize(query: Option<&str>) -> Option<String> {
    query.filter(|q| !q.is_empty()).map(str::to_lowercase)
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_strings(v, out)),
        serde_json::Value::Object(map) => map.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

fn event_text(event: &Event) -> Vec<String> {
    let mut out = vec![event.payload.kind().to_string()];
    if let Ok(value) = serde_json::to_value(&event.payload) {
        if let Some(payload) = value.get("payload") {
            collect_strings(payload, &mut out);
        }
    }
    out
}

fn newest_first(events: &mut [&Event]) {
    events.sort_by(|a, b| (b.ts_ms, b.seq, &b.id).cmp(&(a.ts_ms, a.seq, &a.id)));
}

fn truncate<T>(mut items: Vec<T>, limit: Option<usize>) -> Vec<T> {
    if let Some(n) = limit {
        items.truncate(n);
    }
    items
}

pub fn filter_events(
    events: &[Event],
    filter: &EventFilter,
    query: Option<&str>,
    limit: Option<usize>,
) -> Vec<Event> {
    let q = normalize(query);
    let mut hits: Vec<&Event> = events
        .iter()
        .filter(|e| {
            any_match(&filter.jobs, e.job_id.as_str())
                && stage_match(&filter.stages, e.stage_id.as_deref())
                && any_match(&filter.kinds, e.payload.kind())
                && any_match(&filter.actors, e.actor.as_str())
                && in_range(e.ts_ms, filter.since_ms, filter.until_ms)
                && q.as_deref()
                    .map_or(true, |q| event_text(e).iter().any(|t| contains_ci(t, q)))
        })
        .collect();
    newest_first(&mut hits);
    truncate(hits.into_iter().cloned().collect(), limit)
}

pub fn filter_artifacts(
    events: &[Event],
    filter: &ArtifactFilter,
    query: Option<&str>,
    limit: Option<usize>,
    all_jobs: &[JobId],
) -> Vec<ArtifactItem> {
    let q = normalize(query);
    let mut hits: Vec<(&Event, &str, &str, &str)> = Vec::new();
    for e in events {
        let EventPayload::ArtifactRecorded { kind, path, sha256 } = &e.payload else {
            continue;
        };
        let ok = any_match(&filter.jobs, e.job_id.as_str())
            && stage_match(&filter.stages, e.stage_id.as_deref())
            && any_match(&filter.kinds, kind)
            && (filter.paths.is_empty() || filter.paths.iter().any(|p| path.contains(p.as_str())))
            && in_range(e.ts_ms, filter.since_ms, filter.until_ms)
            && q.as_deref()
                .map_or(true, |q| contains_ci(kind, q) || contains_ci(path, q));
        if ok {
            hits.push((e, kind, path, sha256));
        }
    }
    hits.sort_by(|a, b| (b.0.ts_ms, b.0.seq, &b.0.id).cmp(&(a.0.ts_ms, a.0.seq, &a.0.id)));
    let items = hits
        .into_iter()
        .map(|(e, kind, path, sha256)| ArtifactItem {
            event_id: e.id.clone(),
            job_id: e.job_id.clone(),
            short_id: short_id(&e.job_id, all_jobs),
            stage_id: e.stage_id.clone(),
            ts_ms: e.ts_ms,
            kind: kind.to_string(),
            path: path.to_string(),
            sha256: sha256.to_string(),
        })
        .collect();
    truncate(items, limit)
}

pub fn filter_inbox(
    events: &[Event],
    filter: &InboxFilter,
    query: Option<&str>,
    limit: Option<usize>,
    all_jobs: &[JobId],
) -> Vec<InboxItem> {
    let q = normalize(query);
    let mut hits: Vec<(&Event, &str, &str, &str, Option<u8>)> = Vec::new();
    for e in events {
        let EventPayload::FeedbackRecorded {
            kind,
            severity,
            text,
            confidence,
        } = &e.payload
        else {
            continue;
        };
        let ok = any_match(&filter.jobs, e.job_id.as_str())
            && stage_match(&filter.stages, e.stage_id.as_deref())
            && any_match(&filter.kinds, kind)
            && any_match(&filter.severities, severity)
            && filter
                .min_confidence
                .map_or(true, |min| confidence.is_some_and(|c| c >= min))
            && in_range(e.ts_ms, filter.since_ms, filter.until_ms)
            && q.as_deref().map_or(true, |q| {
                contains_ci(kind, q) || contains_ci(severity, q) || contains_ci(text, q)
            });
        if ok {
            hits.push((e, kind, severity, text, *confidence));
        }
    }
    hits.sort_by(|a, b| (b.0.ts_ms, b.0.seq, &b.0.id).cmp(&(a.0.ts_ms, a.0.seq, &a.0.id)));
    let items = hits
        .into_iter()
        .map(|(e, kind, severity, text, confidence)| InboxItem {
            event_id: e.id.clone(),
            job_id: e.job_id.clone(),
            short_id: short_id(&e.job_id, all_jobs),
            stage_id: e.stage_id.clone(),
            ts_ms: e.ts_ms,
            kind: kind.to_string(),
            severity: severity.to_string(),
            text: text.to_string(),
            confidence,
        })
        .collect();
    truncate(items, limit)
}

/// Parse a `--since`/`--until` value: RFC 3339 or a relative age (`90s`, `15m`,
/// `2h`, `3d`, `1w`) counted back from `now_ms`. Returns epoch milliseconds.
pub fn parse_when(input: &str, now_ms: i64) -> Result<i64, String> {
    let s = input.trim();
    if let Some(ms) = parse_relative(s) {
        return Ok(now_ms - ms);
    }
    parse_rfc3339(s).ok_or_else(|| {
        format!("cannot parse time `{input}`: use RFC 3339 (2026-01-02T03:04:05Z) or 2h, 3d")
    })
}

fn parse_relative(s: &str) -> Option<i64> {
    let unit = s.chars().last()?;
    let per: i64 = match unit {
        's' => 1_000,
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        'w' => 604_800_000,
        _ => return None,
    };
    let n: i64 = s[..s.len() - 1].parse().ok().filter(|n| *n >= 0)?;
    n.checked_mul(per)
}

fn num(s: &str, range: std::ops::RangeInclusive<i64>) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().filter(|n| range.contains(n))
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    if !s.is_ascii() || s.len() < 20 {
        return None;
    }
    let (date, rest) = s.split_once(['T', 't'])?;
    let mut d = date.split('-');
    let year = num(d.next()?, 0..=9999)?;
    let month = num(d.next()?, 1..=12)?;
    let day = num(d.next()?, 1..=31)?;
    if d.next().is_some() {
        return None;
    }
    let (time, offset_s) = if let Some(t) = rest.strip_suffix(['Z', 'z']) {
        (t, 0)
    } else {
        let idx = rest.rfind(['+', '-'])?;
        let (t, off) = rest.split_at(idx);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let (oh, om) = off[1..].split_once(':')?;
        (t, sign * (num(oh, 0..=23)? * 3600 + num(om, 0..=59)? * 60))
    };
    let (hms, frac) = match time.split_once('.') {
        Some((h, f)) => (h, Some(f)),
        None => (time, None),
    };
    let mut t = hms.split(':');
    let hour = num(t.next()?, 0..=23)?;
    let minute = num(t.next()?, 0..=59)?;
    let second = num(t.next()?, 0..=60)?;
    if t.next().is_some() {
        return None;
    }
    let millis = match frac {
        None => 0,
        Some(f) => {
            num(f, 0..=i64::MAX)?;
            let padded: String = f.chars().chain("000".chars()).take(3).collect();
            padded.parse::<i64>().ok()?
        }
    };
    let days = days_from_civil(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second - offset_s;
    Some(secs * 1000 + millis)
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Actor;

    fn ev(n: u64, job: &str, stage: Option<&str>, actor: Actor, payload: EventPayload) -> Event {
        Event {
            id: format!("e{n}"),
            job_id: JobId::new(job),
            stage_id: stage.map(str::to_string),
            seq: n,
            ts_ms: n as i64 * 1000,
            actor,
            payload,
        }
    }

    fn fb(kind: &str, sev: &str, text: &str, conf: Option<u8>) -> EventPayload {
        EventPayload::FeedbackRecorded {
            kind: kind.into(),
            severity: sev.into(),
            text: text.into(),
            confidence: conf,
        }
    }

    fn art(kind: &str, path: &str) -> EventPayload {
        EventPayload::ArtifactRecorded {
            kind: kind.into(),
            path: path.into(),
            sha256: "abc".into(),
        }
    }

    fn sample() -> Vec<Event> {
        vec![
            ev(
                1,
                "JOBA",
                None,
                Actor::Human,
                EventPayload::JobSubmitted {
                    prompt: "Fix the Login bug".into(),
                    repo: None,
                    priority: Default::default(),
                },
            ),
            ev(
                2,
                "JOBA",
                Some("s1"),
                Actor::Daemon,
                EventPayload::StageFailed {
                    reason: "boom".into(),
                },
            ),
            ev(
                3,
                "JOBB",
                Some("s2"),
                Actor::Agent,
                fb("assumption", "high", "Uses OAuth", Some(80)),
            ),
            ev(
                4,
                "JOBB",
                Some("s2"),
                Actor::Agent,
                fb("risk", "low", "Slow scan", None),
            ),
            ev(
                5,
                "JOBA",
                Some("s1"),
                Actor::Agent,
                art("handoff", "docs/spec.md"),
            ),
            ev(
                6,
                "JOBB",
                Some("s2"),
                Actor::Agent,
                art("diff", "src/auth/mod.rs"),
            ),
            ev(
                7,
                "JOBB",
                Some("s3"),
                Actor::Agent,
                fb("question", "high", "Which DB?", Some(30)),
            ),
        ]
    }

    fn ids(events: &[Event]) -> Vec<&str> {
        events.iter().map(|e| e.id.as_str()).collect()
    }

    #[test]
    fn empty_filter_returns_everything_newest_first() {
        let out = filter_events(&sample(), &EventFilter::default(), None, None);
        assert_eq!(ids(&out), ["e7", "e6", "e5", "e4", "e3", "e2", "e1"]);
    }

    #[test]
    fn each_event_property_alone_narrows() {
        let ev = sample();
        let f = |f: EventFilter| ids(&filter_events(&ev, &f, None, None)).join(",");
        assert_eq!(
            f(EventFilter {
                jobs: vec!["JOBA".into()],
                ..Default::default()
            }),
            "e5,e2,e1"
        );
        assert_eq!(
            f(EventFilter {
                stages: vec!["s3".into()],
                ..Default::default()
            }),
            "e7"
        );
        assert_eq!(
            f(EventFilter {
                kinds: vec!["stage-failed".into()],
                ..Default::default()
            }),
            "e2"
        );
        assert_eq!(
            f(EventFilter {
                actors: vec!["daemon".into()],
                ..Default::default()
            }),
            "e2"
        );
        assert_eq!(
            f(EventFilter {
                since_ms: Some(3000),
                until_ms: Some(4000),
                ..Default::default()
            }),
            "e4,e3"
        );
    }

    #[test]
    fn different_properties_and_same_property_or() {
        let ev = sample();
        let and = EventFilter {
            jobs: vec!["JOBB".into()],
            actors: vec!["agent".into()],
            kinds: vec!["feedback-recorded".into()],
            ..Default::default()
        };
        assert_eq!(
            ids(&filter_events(&ev, &and, None, None)),
            ["e7", "e4", "e3"]
        );
        let or = EventFilter {
            kinds: vec!["stage-failed".into(), "job-submitted".into()],
            ..Default::default()
        };
        assert_eq!(ids(&filter_events(&ev, &or, None, None)), ["e2", "e1"]);
        let disjoint = EventFilter {
            jobs: vec!["JOBA".into()],
            stages: vec!["s3".into()],
            ..Default::default()
        };
        assert!(filter_events(&ev, &disjoint, None, None).is_empty());
    }

    #[test]
    fn search_is_case_insensitive_substring_anded_with_filter() {
        let ev = sample();
        let out = filter_events(&ev, &EventFilter::default(), Some("LOGIN"), None);
        assert_eq!(ids(&out), ["e1"]);
        let by_kind = filter_events(&ev, &EventFilter::default(), Some("stage-fail"), None);
        assert_eq!(ids(&by_kind), ["e2"]);
        let anded = EventFilter {
            jobs: vec!["JOBB".into()],
            ..Default::default()
        };
        assert!(filter_events(&ev, &anded, Some("login"), None).is_empty());
        assert!(filter_events(&ev, &EventFilter::default(), Some("zzz"), None).is_empty());
    }

    #[test]
    fn empty_query_is_no_query_and_limit_truncates_after_filtering() {
        let ev = sample();
        assert_eq!(
            filter_events(&ev, &EventFilter::default(), Some(""), None).len(),
            7
        );
        let f = EventFilter {
            jobs: vec!["JOBB".into()],
            ..Default::default()
        };
        assert_eq!(ids(&filter_events(&ev, &f, None, Some(2))), ["e7", "e6"]);
    }

    #[test]
    fn artifacts_filter_and_search() {
        let ev = sample();
        let all = [JobId::new("JOBA"), JobId::new("JOBB")];
        let run = |f: ArtifactFilter, q: Option<&str>| -> Vec<String> {
            filter_artifacts(&ev, &f, q, None, &all)
                .into_iter()
                .map(|a| a.event_id)
                .collect()
        };
        assert_eq!(run(ArtifactFilter::default(), None), ["e6", "e5"]);
        assert_eq!(
            run(
                ArtifactFilter {
                    kinds: vec!["diff".into()],
                    ..Default::default()
                },
                None
            ),
            ["e6"]
        );
        assert_eq!(
            run(
                ArtifactFilter {
                    paths: vec!["src/auth".into()],
                    ..Default::default()
                },
                None
            ),
            ["e6"]
        );
        assert_eq!(
            run(
                ArtifactFilter {
                    jobs: vec!["JOBA".into()],
                    ..Default::default()
                },
                None
            ),
            ["e5"]
        );
        assert_eq!(
            run(
                ArtifactFilter {
                    stages: vec!["s2".into()],
                    ..Default::default()
                },
                None
            ),
            ["e6"]
        );
        assert_eq!(
            run(
                ArtifactFilter {
                    since_ms: Some(6000),
                    ..Default::default()
                },
                None
            ),
            ["e6"]
        );
        assert_eq!(run(ArtifactFilter::default(), Some("SPEC.md")), ["e5"]);
        assert_eq!(run(ArtifactFilter::default(), Some("handoff")), ["e5"]);
        assert!(run(
            ArtifactFilter {
                kinds: vec!["diff".into()],
                ..Default::default()
            },
            Some("spec")
        )
        .is_empty());
    }

    #[test]
    fn inbox_filter_search_and_missing_confidence() {
        let ev = sample();
        let all = [JobId::new("JOBA"), JobId::new("JOBB")];
        let run = |f: InboxFilter, q: Option<&str>| -> Vec<String> {
            filter_inbox(&ev, &f, q, None, &all)
                .into_iter()
                .map(|a| a.event_id)
                .collect()
        };
        assert_eq!(run(InboxFilter::default(), None), ["e7", "e4", "e3"]);
        assert_eq!(
            run(
                InboxFilter {
                    severities: vec!["high".into()],
                    ..Default::default()
                },
                None
            ),
            ["e7", "e3"]
        );
        assert_eq!(
            run(
                InboxFilter {
                    kinds: vec!["risk".into(), "question".into()],
                    ..Default::default()
                },
                None
            ),
            ["e7", "e4"]
        );
        assert_eq!(
            run(
                InboxFilter {
                    min_confidence: Some(50),
                    ..Default::default()
                },
                None
            ),
            ["e3"]
        );
        assert_eq!(
            run(
                InboxFilter {
                    min_confidence: Some(0),
                    ..Default::default()
                },
                None
            ),
            ["e7", "e3"]
        );
        assert_eq!(
            run(
                InboxFilter {
                    stages: vec!["s3".into()],
                    ..Default::default()
                },
                None
            ),
            ["e7"]
        );
        assert_eq!(
            run(
                InboxFilter {
                    until_ms: Some(3000),
                    ..Default::default()
                },
                None
            ),
            ["e3"]
        );
        assert_eq!(run(InboxFilter::default(), Some("oauth")), ["e3"]);
        assert_eq!(run(InboxFilter::default(), Some("HIGH")), ["e7", "e3"]);
        assert_eq!(
            run(
                InboxFilter {
                    severities: vec!["low".into()],
                    ..Default::default()
                },
                Some("oauth")
            ),
            Vec::<String>::new()
        );
        let item = &filter_inbox(&ev, &InboxFilter::default(), None, Some(1), &all)[0];
        assert_eq!(item.text, "Which DB?");
        assert_eq!(item.job_id.as_str(), "JOBB");
        assert!(!item.short_id.is_empty());
    }

    #[test]
    fn inverted_range_is_rejected() {
        assert!(check_range(Some(5), Some(4)).is_err());
        assert!(check_range(Some(4), Some(4)).is_ok());
        assert!(check_range(None, Some(4)).is_ok());
    }

    #[test]
    fn parses_rfc3339_and_relative_times() {
        assert_eq!(parse_when("1970-01-01T00:00:00Z", 0), Ok(0));
        assert_eq!(parse_when("2026-01-02T03:04:05Z", 0), Ok(1_767_323_045_000));
        assert_eq!(
            parse_when("2026-01-02T03:04:05.250Z", 0),
            Ok(1_767_323_045_250)
        );
        assert_eq!(
            parse_when("2026-01-02T05:04:05+02:00", 0),
            Ok(1_767_323_045_000)
        );
        assert_eq!(
            parse_when("2026-01-02T01:04:05-02:00", 0),
            Ok(1_767_323_045_000)
        );
        assert_eq!(parse_when("2h", 10_000_000), Ok(10_000_000 - 7_200_000));
        assert_eq!(
            parse_when("3d", 1_000_000_000),
            Ok(1_000_000_000 - 259_200_000)
        );
        for bad in [
            "",
            "yesterday",
            "2026-13-01T00:00:00Z",
            "2026-01-02",
            "h",
            "-2h",
        ] {
            assert!(parse_when(bad, 0).is_err(), "{bad}");
        }
    }
}
