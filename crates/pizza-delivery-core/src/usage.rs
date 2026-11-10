use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct Usage {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cost_microusd: Option<u64>,
    #[serde(default)]
    pub wall_ms: Option<u64>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub candidate_position: Option<u32>,
}
