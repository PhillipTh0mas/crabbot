use serde::{Deserialize, Serialize};
use serde_json::json;

/// Append-only events persisted as JSONL in `transcripts/<session_id>.jsonl`.
///
/// Design notes (MVP):
/// - Keep this stable early; migrations later are painful.
/// - Add fields rather than changing meanings.
/// - Prefer explicit event variants over ad-hoc JSON blobs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum TranscriptEvent {
    User(UserEvent),
    Assistant(AssistantEvent),

    // Reserved for next steps (tool calling, summaries, etc.)
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    CompactionSummary(CompactionSummaryEvent),
    CustomMessage(CustomMessageEvent),
    CustomNote(CustomNoteEvent),
}

impl TranscriptEvent {
    pub fn user(body: impl Into<String>) -> Self {
        Self::User(UserEvent {
            ts_ms: now_ms(),
            body: body.into(),
        })
    }

    pub fn assistant(body: impl Into<String>) -> Self {
        Self::Assistant(AssistantEvent {
            ts_ms: now_ms(),
            body: body.into(),
        })
    }

    pub fn tool_call(
        tool: impl Into<String>,
        call_id: impl Into<String>,
        args_json: serde_json::Value,
    ) -> Self {
        Self::ToolCall(ToolCallEvent {
            ts_ms: now_ms(),
            tool: tool.into(),
            call_id: call_id.into(),
            args_json,
        })
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        ok: bool,
        result_json: serde_json::Value,
        error: Option<String>,
    ) -> Self {
        Self::ToolResult(ToolResultEvent {
            ts_ms: now_ms(),
            call_id: call_id.into(),
            ok,
            result_json,
            error,
        })
    }

    pub fn compaction_summary(summary: impl Into<String>, covers_up_to_ts_ms: i64) -> Self {
        Self::CompactionSummary(CompactionSummaryEvent {
            ts_ms: now_ms(),
            covers_up_to_ts_ms,
            summary: summary.into(),
        })
    }

    /// In-context message that will be included when reconstructing prompt history.
    pub fn custom_message(role: impl Into<String>, body: impl Into<String>) -> Self {
        Self::CustomMessage(CustomMessageEvent {
            ts_ms: now_ms(),
            role: role.into(),
            body: body.into(),
        })
    }

    /// Out-of-context note: operational metadata, counters, debug, etc.
    pub fn custom_note(key: impl Into<String>, value: serde_json::Value) -> Self {
        Self::CustomNote(CustomNoteEvent {
            ts_ms: now_ms(),
            key: key.into(),
            value,
        })
    }

    pub fn ts_ms(&self) -> i64 {
        match self {
            TranscriptEvent::User(e) => e.ts_ms,
            TranscriptEvent::Assistant(e) => e.ts_ms,
            TranscriptEvent::ToolCall(e) => e.ts_ms,
            TranscriptEvent::ToolResult(e) => e.ts_ms,
            TranscriptEvent::CompactionSummary(e) => e.ts_ms,
            TranscriptEvent::CustomMessage(e) => e.ts_ms,
            TranscriptEvent::CustomNote(e) => e.ts_ms,
        }
    }

    pub fn to_prompt_messages(&self) -> Vec<serde_json::Value> {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        match self {
            TranscriptEvent::User(u) => {
                messages.push(json!({"role":"user","content": u.body}));
            }
            TranscriptEvent::Assistant(a) => {
                messages.push(json!({"role":"assistant","content": a.body}));
            }
            TranscriptEvent::CompactionSummary(cs) => {
                // Summaries are context. Put them as a system note inside the conversation.
                messages
                    .push(json!({"role":"system","content": format!("Summary: {}", cs.summary)}));
            }
            TranscriptEvent::CustomMessage(cm) => {
                // In-context message. Map role if it's one of system/user/assistant.
                let role = match cm.role.as_str() {
                    "system" | "user" | "assistant" | "tool" => cm.role.as_str(),
                    _ => "system",
                };
                messages.push(json!({"role": role, "content": cm.body}));
            }

            // Tool events:
            // For OpenAI-compatible format, tool results must be role="tool".
            // Tool calls are represented in the assistant response object, not as a message you send.
            // When rebuilding history, we can keep tool results so the model sees them.
            TranscriptEvent::ToolResult(tr) => {
                // content must be a string
                let content_json = if tr.ok {
                    tr.result_json.clone()
                } else {
                    json!({
                        "ok": false,
                        "error": tr.error.clone().unwrap_or_else(|| "tool error".to_string()),
                        "result": tr.result_json
                    })
                };

                messages.push(json!({
                    "role":"tool",
                    "tool_call_id": tr.call_id,
                    "content": serde_json::to_string(&content_json).unwrap_or_else(|_| "{}".into())
                }));
            }
            TranscriptEvent::ToolCall(tc) => {
                let args_str = serde_json::to_string(&tc.args_json).unwrap_or_else(|_| "{}".into());

                messages.push(json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": tc.call_id,
                        "type": "function",
                        "function": {
                            "name": tc.tool,
                            "arguments": args_str
                        }
                    }]
                }));
            }

            // Explicitly out-of-context.
            TranscriptEvent::CustomNote(_) => {}
        }

        messages
    }

    pub fn vec_to_prompt_messages(events: &[TranscriptEvent]) -> Vec<serde_json::Value> {
        events
            .iter()
            .map(|event| event.to_prompt_messages())
            .flatten()
            .collect::<Vec<serde_json::Value>>()
    }

    pub fn estimate_prompt_chars(events: &[TranscriptEvent], user_text: &str) -> usize {
        let n = user_text.len();

        let messages = TranscriptEvent::vec_to_prompt_messages(events);
        //vec to string and cnt
        let messages_str = messages
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<String>>();
        let messages_cnt = messages_str.iter().map(|m| m.len()).sum::<usize>();

        n + messages_cnt
    }

    pub fn render_events_for_compaction(events: &[TranscriptEvent]) -> String {
        const MAX_LINE_CHARS: usize = 800;
        const MAX_TOOL_JSON_CHARS: usize = 1200;

        fn trunc(s: &str, max: usize) -> String {
            let s = s.trim();
            if s.chars().count() <= max {
                s.to_string()
            } else {
                let mut out = String::new();
                for (i, ch) in s.chars().enumerate() {
                    if i >= max {
                        break;
                    }
                    out.push(ch);
                }
                out.push_str("…");
                out
            }
        }

        fn trunc_json(v: &serde_json::Value, max: usize) -> String {
            let s = serde_json::to_string(v).unwrap_or_else(|_| "{}".into());
            if s.chars().count() <= max {
                s
            } else {
                trunc(&s, max)
            }
        }

        let mut out = String::new();

        for e in events {
            match e {
                TranscriptEvent::User(u) => {
                    out.push_str("User: ");
                    out.push_str(&trunc(&u.body, MAX_LINE_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::Assistant(a) => {
                    out.push_str("Assistant: ");
                    out.push_str(&trunc(&a.body, MAX_LINE_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::ToolCall(tc) => {
                    out.push_str("ToolCall: ");
                    out.push_str(&tc.tool);
                    out.push_str(" id=");
                    out.push_str(&tc.call_id);
                    out.push_str(" args=");
                    out.push_str(&trunc_json(&tc.args_json, MAX_TOOL_JSON_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::ToolResult(tr) => {
                    out.push_str("ToolResult: id=");
                    out.push_str(&tr.call_id);
                    out.push_str(" ok=");
                    out.push_str(if tr.ok { "true" } else { "false" });

                    if let Some(err) = tr
                        .error
                        .as_ref()
                        .map(|s| s.trim())
                        .filter(|s| !s.is_empty())
                    {
                        out.push_str(" error=");
                        out.push_str(&trunc(err, 400));
                    }

                    out.push_str(" result=");
                    out.push_str(&trunc_json(&tr.result_json, MAX_TOOL_JSON_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::CompactionSummary(cs) => {
                    // Should not generally appear here except possibly at index 0,
                    // but if it does, include it plainly.
                    out.push_str("Summary: ");
                    out.push_str(&trunc(&cs.summary, MAX_LINE_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::CustomMessage(cm) => {
                    out.push_str(cm.role.as_str());
                    out.push_str(": ");
                    out.push_str(&trunc(&cm.body, MAX_LINE_CHARS));
                    out.push('\n');
                }
                TranscriptEvent::CustomNote(_) => {
                    // explicitly out-of-context
                }
            }
        }

        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UserEvent {
    pub ts_ms: i64,
    pub body: String,
    // Future-proofing: add sender/thread/attachments here when you add channels.
    // pub sender: Option<Sender>,
    // pub thread: Option<Thread>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AssistantEvent {
    pub ts_ms: i64,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallEvent {
    pub ts_ms: i64,
    pub tool: String,
    pub call_id: String,
    pub args_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResultEvent {
    pub ts_ms: i64,
    pub call_id: String,
    pub ok: bool,
    pub result_json: serde_json::Value,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CompactionSummaryEvent {
    pub ts_ms: i64,
    /// Summarizes everything up to (and including) this timestamp.
    pub covers_up_to_ts_ms: i64,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomMessageEvent {
    pub ts_ms: i64,
    /// e.g. "system", "developer", "user", "assistant"
    pub role: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CustomNoteEvent {
    pub ts_ms: i64,
    pub key: String,
    pub value: serde_json::Value,
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0));
    // as_millis is u128; clamp to i64
    let ms = dur.as_millis();
    if ms > i64::MAX as u128 {
        i64::MAX
    } else {
        ms as i64
    }
}
