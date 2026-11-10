use serde::{Deserialize, Serialize};
use thiserror::Error;

const MIN_SHORT_LEN: usize = 8;
const LAST: &str = "@last";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(transparent)]
#[ts(export)]
pub struct JobId(String);

impl JobId {
    pub fn new(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    #[error("no job matches '{0}'")]
    NotFound(String),
    #[error("'{reference}' is ambiguous: {} matches", .matches.len())]
    Ambiguous {
        reference: String,
        matches: Vec<JobId>,
    },
}

pub fn resolve(reference: &str, ids: &[JobId]) -> Result<JobId, ResolveError> {
    if reference == LAST {
        return ids
            .iter()
            .max()
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(reference.to_owned()));
    }
    let needle = reference.to_ascii_uppercase();
    let mut matches: Vec<JobId> = ids
        .iter()
        .filter(|id| !needle.is_empty() && id.as_str().starts_with(&needle))
        .cloned()
        .collect();
    matches.sort();
    match matches.as_slice() {
        [] => Err(ResolveError::NotFound(reference.to_owned())),
        [only] => Ok(only.clone()),
        _ => match matches.iter().find(|id| id.as_str() == needle) {
            Some(exact) => Ok(exact.clone()),
            None => Err(ResolveError::Ambiguous {
                reference: reference.to_owned(),
                matches,
            }),
        },
    }
}

pub fn short_id(id: &JobId, all: &[JobId]) -> String {
    let full = id.as_str();
    let len = (MIN_SHORT_LEN..=full.len())
        .find(|&n| {
            let prefix = &full[..n];
            all.iter()
                .filter(|other| *other != id && other.as_str().starts_with(prefix))
                .count()
                == 0
        })
        .unwrap_or(full.len());
    full[..len].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(raw: &[&str]) -> Vec<JobId> {
        raw.iter().map(|s| JobId::new(*s)).collect()
    }

    #[test]
    fn unique_prefix_resolves_case_insensitively() {
        let all = ids(&["01HAAAAAAA0000000000000001", "01HBBBBBBB0000000000000002"]);
        assert_eq!(resolve("01hb", &all), Ok(all[1].clone()));
    }

    #[test]
    fn ambiguous_prefix_lists_every_match() {
        let all = ids(&["01HAAAAAAA0000000000000001", "01HAAAAAAB0000000000000002"]);
        assert_eq!(
            resolve("01HAAAAAAA", &all[..1]),
            Ok(all[0].clone()),
            "sanity"
        );
        assert_eq!(
            resolve("01HAAAAAA", &all),
            Err(ResolveError::Ambiguous {
                reference: "01HAAAAAA".into(),
                matches: all.clone()
            })
        );
    }

    #[test]
    fn unknown_prefix_is_not_found() {
        let all = ids(&["01HAAAAAAA0000000000000001"]);
        assert_eq!(
            resolve("ZZZ", &all),
            Err(ResolveError::NotFound("ZZZ".into()))
        );
        assert_eq!(resolve("", &all), Err(ResolveError::NotFound("".into())));
    }

    #[test]
    fn at_last_is_the_greatest_id() {
        let all = ids(&["01HAAAAAAA0000000000000001", "01HCCCCCCC0000000000000003"]);
        assert_eq!(resolve("@last", &all), Ok(all[1].clone()));
        assert_eq!(
            resolve("@last", &[]),
            Err(ResolveError::NotFound("@last".into()))
        );
    }

    #[test]
    fn short_id_is_the_shortest_unique_prefix_of_at_least_eight_chars() {
        let all = ids(&[
            "01HAAAAAAA0000000000000001",
            "01HAAAAAAA0000000000000002",
            "01HBBBBBBB0000000000000003",
        ]);
        assert_eq!(short_id(&all[2], &all), "01HBBBBB");
        assert_eq!(short_id(&all[0], &all), "01HAAAAAAA0000000000000001");
    }
}
