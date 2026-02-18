use async_trait::async_trait;
use serde_json::json;

use crate::error::Result;
use crate::tools::tool::{Tool, ToolCall, ToolResult, ToolSpec};

#[derive(Debug, Default)]
pub struct NoopTool;

impl NoopTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for NoopTool {
    fn name(&self) -> &str {
        "noop"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "No-op tool for testing wiring; echoes args.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": true
            }),
        }
    }

    async fn call(&self, call: ToolCall) -> Result<ToolResult> {
        Ok(ToolResult::ok(
            call.id,
            call.name,
            json!({ "echo": call.args }),
        ))
    }
}
