use serde::{Deserialize, Serialize};

use crate::tools::tool::ToolCall;

#[derive(Debug, Clone)]
pub enum LlmOutput {
    AssistantText(String),
    ToolCall(ToolCall),
}

#[derive(Debug, Clone)]
pub struct Completion {
    pub outputs: Vec<LlmOutput>,
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}
