// crates/crabbot-runtime/src/tools/tasks.rs

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::tools::tool::{Tool, ToolCall, ToolResult, ToolSpec};

use crate::task::manager::{Task, TaskCreateInput, TaskManager, TaskStatus};

#[derive(Clone, Debug)]
pub struct TasksTool {
    tasks: Arc<TaskManager>,
}

impl TasksTool {
    pub fn new(tasks: Arc<TaskManager>) -> Self {
        Self { tasks }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum TasksArgs {
    Create {
        description: String,

        /// Run every N seconds (0 means "no schedule" -> one-shot immediate).
        #[serde(default)]
        interval_secs: u64,
    },

    Complete {
        task_id: String,
        #[serde(default)]
        output: Option<String>,
    },

    Fail {
        task_id: String,
        error: String,
    },

    Cancel {
        task_id: String,
        #[serde(default)]
        reason: Option<String>,
    },

    Get {
        task_id: String,
    },

    List {
        /// Filter by status (optional)
        #[serde(default)]
        status: Option<String>,
        /// Filter by agent_id (optional)
        #[serde(default)]
        agent_id: Option<String>,
        /// Hard cap
        #[serde(default)]
        limit: Option<usize>,
    },
}

#[derive(Debug, Serialize)]
struct CreateOut {
    task: Task,
}

#[derive(Debug, Serialize)]
struct TaskOut {
    task: Task,
}

#[derive(Debug, Serialize)]
struct ListOut {
    tasks: Vec<Task>,
}

#[async_trait]
impl Tool for TasksTool {
    fn name(&self) -> &str {
        "tasks"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Create and manage persisted tasks. Use `op` to select an operation, and only provide the parameters required/allowed for that operation.".to_string(),
            parameters: json!({
                "oneOf": [
                    {
                        "title": "create",
                        "type": "object",
                        "required": ["op", "agent_id", "description"],
                        "properties": {
                            "op": { "type": "string", "const": "create" },
                            "description": { "type": "string", "description": "Human-readable task description." },
                            "interval_secs": { "type": "integer", "minimum": 0, "default": 0, "description": "If >0, task repeats every N seconds. If 0, one-shot immediate." }
                        },
                        "additionalProperties": false
                    },
                    {
                        "title": "complete",
                        "type": "object",
                        "required": ["op", "task_id"],
                        "properties": {
                            "op": { "type": "string", "const": "complete" },
                            "task_id": { "type": "string", "description": "Task id to mark completed." },
                            "output": { "type": "string", "description": "Optional completion output/result." }
                        },
                        "additionalProperties": false
                    },
                    {
                        "title": "fail",
                        "type": "object",
                        "required": ["op", "task_id", "error"],
                        "properties": {
                            "op": { "type": "string", "const": "fail" },
                            "task_id": { "type": "string", "description": "Task id to mark failed." },
                            "error": { "type": "string", "description": "Failure reason/details." }
                        },
                        "additionalProperties": false
                    },
                    {
                        "title": "cancel",
                        "type": "object",
                        "required": ["op", "task_id"],
                        "properties": {
                            "op": { "type": "string", "const": "cancel" },
                            "task_id": { "type": "string", "description": "Task id to cancel." },
                            "reason": { "type": "string", "description": "Optional cancellation reason." }
                        },
                        "additionalProperties": false
                    },
                    {
                        "title": "get",
                        "type": "object",
                        "required": ["op", "task_id"],
                        "properties": {
                            "op": { "type": "string", "const": "get" },
                            "task_id": { "type": "string", "description": "Task id to fetch." }
                        },
                        "additionalProperties": false
                    },
                    {
                        "title": "list",
                        "type": "object",
                        "required": ["op"],
                        "properties": {
                            "op": { "type": "string", "const": "list" },
                            "status": { "type": "string", "description": "Optional filter: active|completed|failed|canceled" },
                            "agent_id": { "type": "string", "description": "Optional filter by agent id." },
                            "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100, "description": "Max number of tasks to return (capped at 500)." }
                        },
                        "additionalProperties": false
                    }
                ]
            }),
        }
    }

    async fn call(&self, call: ToolCall, session_key: &str) -> Result<ToolResult> {
        let args: TasksArgs = serde_json::from_value(call.args)
            .map_err(|e| Error::bad_request(format!("tasks tool: invalid args schema: {e}")))?;

        match args {
            TasksArgs::Create {
                description,
                interval_secs,
            } => {
                let input = TaskCreateInput {
                    agent_id: "default".to_string(),
                    description,
                    interval_secs,
                    // default: if caller didn’t provide a notify session, use the current session
                    notify_session_key: Some(session_key.to_string()),
                    notify_agent_id: None,
                };

                let task = self.tasks.create(input).await?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(CreateOut { task }).unwrap(),
                ))
            }

            TasksArgs::Complete { task_id, output } => {
                let task = self.tasks.complete(&task_id, output).await?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(TaskOut { task }).unwrap(),
                ))
            }

            TasksArgs::Fail { task_id, error } => {
                let task = self.tasks.fail(&task_id, error).await?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(TaskOut { task }).unwrap(),
                ))
            }

            TasksArgs::Cancel { task_id, reason } => {
                let task = self.tasks.cancel(&task_id, reason).await?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(TaskOut { task }).unwrap(),
                ))
            }

            TasksArgs::Get { task_id } => {
                let task = self
                    .tasks
                    .get(&task_id)
                    .await?
                    .ok_or_else(|| Error::other(format!("task not found: {task_id}")))?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(TaskOut { task }).unwrap(),
                ))
            }

            TasksArgs::List {
                status,
                agent_id,
                limit,
            } => {
                let limit = limit.unwrap_or(100).min(500);

                let status = status.as_deref().map(parse_status).transpose()?;

                let tasks = self.tasks.list(status, agent_id.as_deref(), limit).await?;
                Ok(ToolResult::ok(
                    call.id,
                    call.name,
                    serde_json::to_value(ListOut { tasks }).unwrap(),
                ))
            }
        }
    }
}

fn parse_status(s: &str) -> Result<TaskStatus> {
    match s.trim().to_lowercase().as_str() {
        "active" => Ok(TaskStatus::Active),
        "completed" => Ok(TaskStatus::Completed),
        "failed" => Ok(TaskStatus::Failed),
        "canceled" | "cancelled" => Ok(TaskStatus::Canceled),
        other => Err(Error::bad_request(format!(
            "tasks tool: invalid status: {other}"
        ))),
    }
}
