// src/tools/mod.rs

use std::fmt::Debug;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::error::Result;

/// A single tool call coming from the LLM/runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Tool-specific arguments (JSON object).
    #[serde(default)]
    pub args: Json,
}

/// What a tool returns to the runtime/LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub id: String,
    pub name: String,

    /// Tool result payload. Keep it structured; let the caller stringify if needed.
    #[serde(default)]
    pub output: Json,

    /// Optional human-readable error. Prefer returning `Err` for hard failures and
    /// use this for “soft” tool-level errors you still want to surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ToolResult {
    pub fn ok(id: impl Into<String>, name: impl Into<String>, output: Json) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            output,
            error: None,
        }
    }

    pub fn soft_error(
        id: impl Into<String>,
        name: impl Into<String>,
        msg: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            output: Json::Null,
            error: Some(msg.into()),
        }
    }
}

/// Optional: schema/spec for tool discovery (OpenAI/Anthropic style).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for arguments (object).
    pub parameters: Json,
}

#[async_trait]
pub trait Tool: Debug + Send + Sync {
    fn name(&self) -> &str;
    fn spec(&self) -> ToolSpec;

    /// Execute a tool call.
    async fn call(&self, call: ToolCall) -> Result<ToolResult>;
}
