use clap::builder::Str;
use serde::{Deserialize, Serialize};

use crate::config::Paths;

pub type AgentId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPolicy {
    /// Only these tools are exposed to the model.
    AllowList,
    /// All tools except these are exposed to the model.
    DenyList,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MdInclude {
    /// SOUL.md file in your workspace
    Sould,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: AgentId, // stable identifier, used in routing/requests
    pub display_name: String,
    #[serde(default)]
    pub description: String,

    /// Prompt bootstrap documents.
    #[serde(default)]
    pub md_includes: Vec<MdInclude>,

    /// Tool exposure policy for this agent.
    #[serde(default = "default_tool_policy")]
    pub tool_policy: ToolPolicy,
    #[serde(default)]
    pub tools: Vec<String>, // allowlist or denylist depending on policy

    /// Retrieval / memory behavior.
    #[serde(default = "default_true")]
    pub enable_recall: bool,
    #[serde(default)]
    pub recall_top_k: Option<usize>,
    #[serde(default)]
    pub recall_max_chars: Option<usize>,

    /// LLM loop behavior.
    #[serde(default)]
    pub max_steps: Option<usize>,

    /// Optional model overrides (leave None to use session/model defaults).
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,

    /// Metadata
    #[serde(default)]
    pub created_ts_ms: i64,
    #[serde(default)]
    pub updated_ts_ms: i64,
    #[serde(default)]
    pub version: u64,
    #[serde(default = "default_true")]
    pub enabled: bool,

    pub include_long_term_memory: bool,
    pub include_daily_memory_days: usize, // 0, 1, 2
    pub memory_prompt_max_chars: usize,   // cap injected text
}

fn default_true() -> bool {
    true
}

fn default_tool_policy() -> ToolPolicy {
    ToolPolicy::AllowList
}

/// Patch type for partial updates (API-friendly).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentProfilePatch {
    pub display_name: Option<String>,
    pub description: Option<String>,

    pub md_includes: Option<Vec<MdInclude>>,

    pub tool_policy: Option<ToolPolicy>,
    pub tools: Option<Vec<String>>,

    pub enable_recall: Option<bool>,
    pub recall_top_k: Option<Option<usize>>,
    pub recall_max_chars: Option<Option<usize>>,

    pub max_steps: Option<Option<usize>>,

    pub model: Option<Option<String>>,
    pub temperature: Option<Option<f32>>,
    pub max_output_tokens: Option<Option<u32>>,

    pub enabled: Option<bool>,
}
