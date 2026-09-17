//! Provider-neutral decision types shared by policy engines and executors.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TokenUsage {
    pub input: u64,
    #[serde(default)]
    pub cached_input: u64,
    pub output: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}
