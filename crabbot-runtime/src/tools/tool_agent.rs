// crabbot-runtime/src/tools/tool_agent.rs
//
// Tool Agent Service
//
// Instead of the calling agent receiving full JSON Schema tool specs and making
// structured tool calls directly, it now calls a single generic `use_tool` function
// with a tool name and a natural language prompt. This module houses the "tool agent"
// that receives that prompt, has the full tool schema + tool session context, makes
// the real structured tool call, and returns a plain text summary to the caller.
//
// Flow:
//   Calling agent → use_tool(name, prompt) → ToolAgentService::handle()
//     → builds a mini-session with tool schema + tool session history
//     → LLM completion (with the real tool's spec)
//     → executes the structured tool call
//     → returns plain text summary string

use std::sync::Arc;

use serde_json::json;

use crabbot_shared::api::transcript::TranscriptEvent;

use crate::{
    error::{Error, Result},
    llm::{
        client::Llm,
        types::{Completion, LlmOutput},
    },
    storage::session_store::{ModelOverride, SessionCounters, SessionEntry, SessionFlags},
    tools::{
        registry::ToolRegistry,
        tool::{ToolResult, ToolSpec},
        tool_sessions::ToolSessionStore,
    },
};

/// Maximum number of LLM ↔ tool loops the tool agent is allowed per request.
const TOOL_AGENT_MAX_STEPS: usize = 5;

/// Maximum chars of tool session context to inject into the tool agent prompt.
const TOOL_SESSION_CONTEXT_BUDGET: usize = 3_000;

/// Maximum chars for the final summary returned to the calling agent.
const MAX_SUMMARY_CHARS: usize = 8_000;

// ─── Service ─────────────────────────────────────────────────────────────────

/// The tool agent service. Shared across the runtime — one instance handles
/// all tool agent calls by dispatching to the right tool.
#[derive(Debug)]
pub struct ToolAgentService {
    llm: Arc<dyn Llm>,
    tools: Arc<ToolRegistry>,
    tool_sessions: Arc<ToolSessionStore>,
}

impl ToolAgentService {
    pub fn new(
        llm: Arc<dyn Llm>,
        tools: Arc<ToolRegistry>,
        tool_sessions: Arc<ToolSessionStore>,
    ) -> Self {
        Self {
            llm,
            tools,
            tool_sessions,
        }
    }

    /// Build the single `use_tool` ToolSpec that the calling agent sees.
    /// This replaces all individual tool specs in the caller's context.
    /// The calling agent only knows tool names + short descriptions.
    pub async fn use_tool_spec(&self) -> Option<ToolSpec> {
        if !self.tools.is_enabled() {
            return None;
        }

        let compact = self.tools.specs_compact().await;
        if compact.is_empty() {
            return None;
        }

        // Build a human-readable list of available tools for the description.
        let mut tool_list = String::new();
        for spec in &compact {
            tool_list.push_str(&format!("- `{}`: {}\n", spec.name, spec.description));
        }

        let description = format!(
            "Use a tool by name. Provide the tool name and a natural language description of \
             what you want the tool to do. A specialised tool agent will interpret your request, \
             call the tool with the correct arguments, and return the result as text.\n\n\
             Available tools:\n{tool_list}\n\
             Tips:\n\
             - Be specific about what you want.\n\
             - Include all relevant details (paths, values, filters) in your prompt.\n\
             - The tool agent has memory of past calls and knows common patterns."
        );

        Some(ToolSpec {
            name: "use_tool".to_string(),
            description,
            parameters: json!({
                "type": "object",
                "required": ["tool_name", "prompt"],
                "properties": {
                    "tool_name": {
                        "type": "string",
                        "description": "Name of the tool to use (see list above)."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Natural language description of what you want the tool to do. Be specific."
                    }
                },
                "additionalProperties": false
            }),
        })
    }

    /// Handle a `use_tool` call from the calling agent.
    ///
    /// - `caller_session_key`: the session key of the calling agent (for passing to
    ///   the underlying tool's `call()` method).
    /// - `tool_name`: which tool to invoke.
    /// - `prompt`: the natural language request from the calling agent.
    ///
    /// Returns a plain text summary suitable for injecting back into the caller's
    /// transcript as a tool result.
    pub async fn handle(
        &self,
        caller_session_key: &str,
        tool_name: &str,
        prompt: &str,
    ) -> Result<String> {
        // 1. Look up the real tool.
        let tool = self
            .tools
            .get(tool_name)
            .await
            .ok_or_else(|| Error::tool(format!("unknown tool: {tool_name}")))?;

        let real_spec = tool.spec();

        // 2. Load tool session context (accumulated knowledge about this tool).
        let session_context = self
            .tool_sessions
            .get_tool_context(tool_name, TOOL_SESSION_CONTEXT_BUDGET)
            .await;

        // 3. Build the tool agent's prompt events.
        let events = self.build_tool_agent_prompt(tool_name, &real_spec, &session_context, prompt);

        // 4. Create a synthetic session entry for the tool agent LLM call.
        let synth_session = make_tool_agent_session(tool_name);

        // 5. Run the tool agent loop: LLM → tool call → result → maybe loop.
        let (summary, tool_calls_made) = self
            .run_tool_agent_loop(
                &synth_session,
                events,
                &real_spec,
                tool.as_ref(),
                caller_session_key,
                tool_name,
            )
            .await?;

        // 6. If the tool agent never actually called the tool, say so.
        if tool_calls_made == 0 {
            // The LLM just responded with text without calling the tool.
            // That text is still useful — it might be an explanation of why it can't proceed.
            if summary.trim().is_empty() {
                return Ok(format!(
                    "[Tool agent for '{tool_name}' did not produce any output for request: {prompt}]"
                ));
            }
        }

        Ok(truncate_str(&summary, MAX_SUMMARY_CHARS))
    }

    /// Build the prompt events for the tool agent's LLM call.
    fn build_tool_agent_prompt(
        &self,
        tool_name: &str,
        spec: &ToolSpec,
        session_context: &str,
        user_prompt: &str,
    ) -> Vec<TranscriptEvent> {
        let mut events = Vec::new();

        // System message: tell the tool agent what it is and what tool it operates.
        let mut system = format!(
            "You are a specialised tool agent for the '{}' tool.\n\n\
             Your job is to interpret user requests and call the tool with the correct arguments.\n\
             After calling the tool, summarise the result clearly and concisely for the caller.\n\n\
             IMPORTANT RULES:\n\
             - You MUST call the tool to fulfil the request. Do not just describe what you would do.\n\
             - If the request is ambiguous, make your best interpretation and call the tool.\n\
             - If the tool returns an error, explain what went wrong.\n\
             - Your final response should be a plain text summary of what happened and the result.\n\
             - Do NOT return raw JSON — summarise it into readable text.\n\
             - Be concise but complete.\n\n\
             Tool description: {}\n\n\
             Tool argument schema:\n```json\n{}\n```\n",
            tool_name,
            spec.description,
            serde_json::to_string_pretty(&spec.parameters).unwrap_or_default()
        );

        // Append tool session context if available.
        if !session_context.is_empty() {
            system.push_str(&format!(
                "\n--- Past usage context for this tool ---\n{}\n--- End past usage context ---\n",
                session_context
            ));
        }

        events.push(TranscriptEvent::custom_message("system", system));

        // The user's request becomes the user message.
        events.push(TranscriptEvent::user(user_prompt.to_string()));

        events
    }

    /// Run the tool agent's LLM loop.
    /// Returns (summary_text, number_of_tool_calls_made).
    async fn run_tool_agent_loop(
        &self,
        session: &SessionEntry,
        mut working_events: Vec<TranscriptEvent>,
        spec: &ToolSpec,
        tool: &dyn crate::tools::tool::Tool,
        caller_session_key: &str,
        tool_name: &str,
    ) -> Result<(String, usize)> {
        let tool_specs = vec![spec.clone()];
        let mut final_text = String::new();
        let mut tool_calls_made = 0usize;

        for _step in 0..TOOL_AGENT_MAX_STEPS {
            let completion: Completion = self
                .llm
                .complete(session, &working_events, &tool_specs)
                .await?;

            let mut saw_tool_call = false;

            for out in completion.outputs {
                match out {
                    LlmOutput::AssistantText(text) => {
                        if !final_text.is_empty() {
                            final_text.push('\n');
                        }
                        final_text.push_str(&text);
                        working_events.push(TranscriptEvent::assistant(text));
                    }
                    LlmOutput::ToolCall(tc) => {
                        saw_tool_call = true;
                        tool_calls_made += 1;

                        // Record the call in the tool session.
                        let _ = self.tool_sessions.record_call(tool_name, &tc).await;

                        // Add the tool call to the working events so the LLM
                        // sees it in context.
                        working_events.push(TranscriptEvent::tool_call(
                            tc.name.clone(),
                            tc.id.clone(),
                            tc.args.clone(),
                        ));

                        // Execute the real tool.
                        match tool.call(tc.clone(), caller_session_key).await {
                            Ok(result) => {
                                // Record success.
                                let _ = self
                                    .tool_sessions
                                    .record_result(tool_name, &tc.id, &result.output)
                                    .await;

                                let result_text = format_tool_result(&result);
                                let error_clone = result.error.clone();
                                working_events.push(TranscriptEvent::tool_result(
                                    tc.id.clone(),
                                    true,
                                    result.output,
                                    result.error,
                                ));

                                // If there's an error field, note it.
                                if let Some(ref err) = error_clone {
                                    let _ = self
                                        .tool_sessions
                                        .record_note(
                                            tool_name,
                                            &format!("Soft error on call {}: {}", tc.id, err),
                                        )
                                        .await;
                                }

                                tracing::debug!(
                                    "Tool agent [{tool_name}] call {} → {}",
                                    tc.id,
                                    truncate_str(&result_text, 200)
                                );
                            }
                            Err(err) => {
                                // Record error.
                                let _ = self
                                    .tool_sessions
                                    .record_error(tool_name, &tc.id, &err.to_string())
                                    .await;

                                working_events.push(TranscriptEvent::tool_result(
                                    tc.id.clone(),
                                    false,
                                    json!({}),
                                    Some(err.to_string()),
                                ));

                                tracing::warn!(
                                    "Tool agent [{tool_name}] call {} error: {}",
                                    tc.id,
                                    err
                                );
                            }
                        }
                    }
                }
            }

            if !saw_tool_call {
                // The LLM produced only text — it's done summarising.
                break;
            }
        }

        Ok((final_text, tool_calls_made))
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Create a synthetic SessionEntry for tool agent LLM calls.
/// These don't go through the normal session store — they're ephemeral.
fn make_tool_agent_session(tool_name: &str) -> SessionEntry {
    SessionEntry {
        session_key: format!("tool_agent:{tool_name}"),
        session_id: format!("tool_agent:{tool_name}"),
        model: Some(ModelOverride {
            model: None,            // use default model
            temperature: Some(0.1), // tool agents should be precise
            max_output_tokens: None,
        }),
        flags: SessionFlags {
            compaction_enabled: false, // tool agent sessions are ephemeral per-request
            daily_reset: false,
        },
        counters: SessionCounters::default(),
    }
}

/// Format a ToolResult into a brief text representation for the tool agent
/// to see in its transcript.
fn format_tool_result(result: &ToolResult) -> String {
    let mut out = String::new();

    // Stringify the output value.
    match &result.output {
        serde_json::Value::Null => {
            out.push_str("[no output]");
        }
        serde_json::Value::String(s) => {
            out.push_str(s);
        }
        other => {
            out.push_str(
                &serde_json::to_string_pretty(other).unwrap_or_else(|_| format!("{:?}", other)),
            );
        }
    }

    if let Some(ref err) = result.error {
        out.push_str(&format!("\n[error: {}]", err));
    }

    out
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max_chars {
            break;
        }
        out.push(ch);
    }
    out.push_str("…");
    out
}

// ─── Parsing helper for RunEngine ────────────────────────────────────────────

/// Parse the args of a `use_tool` call into (tool_name, prompt).
pub fn parse_use_tool_args(args: &serde_json::Value) -> Result<(String, String)> {
    let tool_name = args
        .get("tool_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::bad_request("use_tool: missing 'tool_name' argument"))?
        .to_string();

    let prompt = args
        .get("prompt")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::bad_request("use_tool: missing 'prompt' argument"))?
        .to_string();

    if tool_name.trim().is_empty() {
        return Err(Error::bad_request("use_tool: 'tool_name' is empty"));
    }
    if prompt.trim().is_empty() {
        return Err(Error::bad_request("use_tool: 'prompt' is empty"));
    }

    Ok((tool_name, prompt))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_use_tool_args_ok() {
        let args = json!({
            "tool_name": "command",
            "prompt": "list files in /tmp"
        });
        let (name, prompt) = parse_use_tool_args(&args).unwrap();
        assert_eq!(name, "command");
        assert_eq!(prompt, "list files in /tmp");
    }

    #[test]
    fn test_parse_use_tool_args_missing_name() {
        let args = json!({ "prompt": "hello" });
        assert!(parse_use_tool_args(&args).is_err());
    }

    #[test]
    fn test_parse_use_tool_args_missing_prompt() {
        let args = json!({ "tool_name": "command" });
        assert!(parse_use_tool_args(&args).is_err());
    }

    #[test]
    fn test_parse_use_tool_args_empty_name() {
        let args = json!({ "tool_name": "", "prompt": "hello" });
        assert!(parse_use_tool_args(&args).is_err());
    }

    #[test]
    fn test_parse_use_tool_args_empty_prompt() {
        let args = json!({ "tool_name": "command", "prompt": "  " });
        assert!(parse_use_tool_args(&args).is_err());
    }

    #[test]
    fn test_truncate_str_short() {
        assert_eq!(truncate_str("hello", 100), "hello");
    }

    #[test]
    fn test_truncate_str_exact() {
        assert_eq!(truncate_str("hello", 5), "hello");
    }

    #[test]
    fn test_truncate_str_over() {
        assert_eq!(truncate_str("hello world", 5), "hello…");
    }

    #[test]
    fn test_format_tool_result_string() {
        let r = ToolResult::ok("id", "name", json!("some output"));
        let s = format_tool_result(&r);
        assert_eq!(s, "some output");
    }

    #[test]
    fn test_format_tool_result_null() {
        let r = ToolResult::ok("id", "name", json!(null));
        let s = format_tool_result(&r);
        assert_eq!(s, "[no output]");
    }

    #[test]
    fn test_format_tool_result_with_error() {
        let r = ToolResult::soft_error("id", "name", "oops");
        let s = format_tool_result(&r);
        assert!(s.contains("[error: oops]"));
    }
}
