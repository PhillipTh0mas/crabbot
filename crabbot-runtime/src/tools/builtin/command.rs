use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::process::Command;

use crate::error::{Error, Result};
use crate::tools::tool::{Tool, ToolCall, ToolResult, ToolSpec};

#[derive(Debug, Default)]
pub struct CommandTool {
    /// Hard safety cap.
    max_output_bytes: usize,
}

impl CommandTool {
    pub fn new() -> Self {
        Self {
            max_output_bytes: 256 * 1024, // 256 KiB
        }
    }

    pub fn with_max_output_bytes(mut self, n: usize) -> Self {
        self.max_output_bytes = n.max(1024);
        self
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
struct CommandArgs {
    /// Program to execute (absolute or in PATH).
    program: String,

    /// Arguments to pass to the program.
    #[serde(default)]
    args: Vec<String>,

    /// Optional working directory.
    #[serde(default)]
    cwd: Option<String>,

    /// Optional environment variables.
    /// Note: null value removes a var if you decide to implement that later.
    #[serde(default)]
    env: Option<std::collections::BTreeMap<String, String>>,

    /// If true, merges stderr into stdout in the returned output.
    #[serde(default = "default_true")]
    merge_stderr: bool,
}

#[derive(Debug, Serialize)]
struct CommandOutput {
    exit_code: Option<i32>,
    success: bool,
    stdout: String,
    stderr: String,
}

#[async_trait]
impl Tool for CommandTool {
    fn name(&self) -> &str {
        "command"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Run a local command via tokio::process::Command. Returns exit code + stdout/stderr (truncated).".to_string(),
            parameters: json!({
                "type": "object",
                "required": ["program"],
                "properties": {
                    "program": { "type": "string", "description": "Executable name or absolute path. No flags just the binary! Dont reinclude the binary name in args!" },
                    "args": { "type": "array", "items": { "type": "string" }, "default": [] },
                    "cwd": { "type": "string", "description": "Working directory." },
                    "env": { "type": "object", "additionalProperties": { "type": "string" }, "description": "Environment variables to set." },
                    "merge_stderr": { "type": "boolean", "default": false }
                },
                "additionalProperties": false
            }),
        }
    }

    async fn call(&self, call: ToolCall, _session_key: &str) -> Result<ToolResult> {
        // ToolCall.args in your codebase is a String; parse it as JSON.

        let args: CommandArgs = serde_json::from_value(call.args)
            .map_err(|e| Error::bad_request(format!("command tool: invalid args schema: {e}")))?;

        // Build command
        let mut cmd = Command::new(&args.program);
        cmd.args(&args.args);
        cmd.kill_on_drop(true);

        if let Some(cwd) = args.cwd.as_deref() {
            cmd.current_dir(cwd);
        }
        if let Some(env) = args.env.as_ref() {
            cmd.envs(env);
        }

        // Run and capture output
        let out = cmd
            .output()
            .await
            .map_err(|e| Error::tool(format!("command tool: failed to spawn/run: {e}")))?;

        let exit_code = out.status.code();
        let success = out.status.success();

        // Decode + truncate
        let mut stdout = String::from_utf8_lossy(&out.stdout).to_string();
        let mut stderr = String::from_utf8_lossy(&out.stderr).to_string();

        if args.merge_stderr && !stderr.is_empty() {
            stdout.push_str(&stderr);
            stderr.clear();
        }

        truncate_string(&mut stdout, self.max_output_bytes);
        truncate_string(&mut stderr, self.max_output_bytes);

        let payload = CommandOutput {
            exit_code,
            success,
            stdout,
            stderr,
        };

        Ok(ToolResult::ok(
            call.id,
            call.name,
            serde_json::to_value(payload).unwrap(),
        ))
    }
}

fn truncate_string(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    // keep it valid UTF-8 by truncating to a char boundary
    let mut cut = max_bytes;
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n[truncated]\n");
}
