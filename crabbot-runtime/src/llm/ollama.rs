use async_trait::async_trait;
use crabbot_shared::api::transcript::TranscriptEvent;
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value as Json;

use crate::{
    config::{LlmConfig, PromptConfig},
    error::{Error, Result},
    prompt::default_builder::DefaultPromptBuilder,
    storage::session_store::SessionEntry,
    tools::tool::{ToolCall, ToolSpec},
};

use super::{
    client::Llm,
    types::{Completion, LlmOutput, Usage},
};

/// Ollama can expose an OpenAI-compatible API at `/v1`.
/// This client assumes `base_url` points at that (e.g. http://127.0.0.1:11434/v1).
#[derive(Debug)]
pub struct OllamaOpenAiCompat {
    cfg: LlmConfig,
    http: Client,
    prompt_builder: DefaultPromptBuilder,
}

impl OllamaOpenAiCompat {
    pub fn new(cfg: LlmConfig, prompt_cfg: PromptConfig) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
            .build()
            .map_err(|e| Error::other(e.to_string()))?;

        let prompt_builder = DefaultPromptBuilder::new(prompt_cfg);
        Ok(Self {
            cfg,
            http,
            prompt_builder,
        })
    }

    fn effective_model<'a>(&'a self, session: &'a SessionEntry) -> &'a str {
        session
            .model
            .as_ref()
            .and_then(|m| m.model.as_deref())
            .unwrap_or(&self.cfg.model)
    }

    fn effective_temperature(&self, session: &SessionEntry) -> f32 {
        session
            .model
            .as_ref()
            .and_then(|m| m.temperature)
            .unwrap_or(self.cfg.temperature)
    }

    fn effective_max_tokens(&self, session: &SessionEntry) -> u32 {
        session
            .model
            .as_ref()
            .and_then(|m| m.max_output_tokens)
            .unwrap_or(self.cfg.max_output_tokens)
    }
}

#[async_trait]
impl Llm for OllamaOpenAiCompat {
    async fn compact(
        &self,
        session: &SessionEntry,
        compacted_events: &[TranscriptEvent],
    ) -> Result<String> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );

        // Build compaction messages (system + user) using the same prompt builder.
        let messages = self
            .prompt_builder
            .build_compact_messages(session, compacted_events);

        // Keep compaction deterministic-ish: low temperature.
        let mut body = serde_json::json!({
            "model": self.effective_model(session),
            "messages": messages,
            "temperature": 0.2_f32,
            "max_tokens": self.effective_max_tokens(session),
        });

        // No tools during compaction.
        // If you want to be extra strict and your backend supports it, you could request JSON mode,
        // but OpenAI-compat in Ollama is not guaranteed to support response_format reliably.

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::llm(format!("request failed: {e}")))?;

        let text = match resp.error_for_status() {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(e) => return Err(Error::llm(e.to_string())),
        };

        let parsed: ChatCompletionsResponse =
            serde_json::from_str(&text).map_err(|e| Error::llm(format!("bad json: {e}")))?;

        let completion = parsed.into_completion();

        // Extract assistant text only.
        let mut summary = String::new();
        for out in completion.outputs {
            if let LlmOutput::AssistantText(t) = out {
                summary.push_str(t.trim());
            }
        }

        // Enforce char budget (retry once by asking to shorten; then hard-trim).
        if !self.prompt_builder.is_valid_compaction(&summary) {
            // Retry once with a stricter instruction.
            let mut retry_messages = self
                .prompt_builder
                .build_compact_messages(session, compacted_events);

            retry_messages.push(serde_json::json!({
                    "role": "user",
                    "content": format!("Shorten the summary to <= {} characters. Output ONLY the summary text.", self.prompt_builder.get_max_chars())
                }));

            body["messages"] = Json::Array(retry_messages);

            let resp2 = self
                .http
                .post(&url)
                .json(&body)
                .send()
                .await
                .map_err(|e| Error::llm(format!("request failed: {e}")))?;

            let status2 = resp2.status();
            let text2 = resp2.text().await.unwrap_or_default();
            if status2.is_success() {
                if let Ok(parsed2) = serde_json::from_str::<ChatCompletionsResponse>(&text2) {
                    let completion2 = parsed2.into_completion();
                    let mut s2 = String::new();
                    for out in completion2.outputs {
                        if let LlmOutput::AssistantText(t) = out {
                            s2.push_str(t.trim());
                        }
                    }
                    if !s2.trim().is_empty() {
                        summary = s2;
                    }
                }
            }
        }

        // Final hard enforcement.
        summary = summary.trim().to_string();
        if !self.prompt_builder.is_valid_compaction(&summary) {
            summary = self.prompt_builder.trim_summary(&summary);
        }

        Ok(summary)
    }

    async fn complete(
        &self,
        session: &SessionEntry,
        events: &[TranscriptEvent],
        tools: &[ToolSpec],
    ) -> Result<Completion> {
        let url = format!(
            "{}/chat/completions",
            self.cfg.base_url.trim_end_matches('/')
        );

        let messages = self.prompt_builder.build_messages(session, events);

        let mut body = serde_json::json!({
            "model": self.effective_model(session),
            "messages": messages,
            "temperature": self.effective_temperature(session),
            "max_tokens": self.effective_max_tokens(session),
        });

        if !tools.is_empty() {
            // OpenAI-style tool schema
            let tool_objs: Vec<Json> = tools
                .into_iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters
                        }
                    })
                })
                .collect();
            body["tools"] = Json::Array(tool_objs);

            // Let model choose. You can force a tool by setting a specific tool_choice.
            body["tool_choice"] = serde_json::json!("auto");
        }

        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::llm(format!("request failed: {e}")))?;

        let text = match resp.error_for_status() {
            Ok(r) => r.text().await.unwrap_or_default(),
            Err(e) => return Err(Error::llm(e.to_string())),
        };

        let parsed: ChatCompletionsResponse =
            serde_json::from_str(&text).map_err(|e| Error::llm(format!("bad json: {e}")))?;

        Ok(parsed.into_completion())
    }
}

// ---------------- OpenAI-compatible response parsing ----------------

#[derive(Debug, Deserialize)]
struct ChatCompletionsResponse {
    #[allow(dead_code)]
    id: Option<String>,
    choices: Vec<Choice>,
    usage: Option<UsageWire>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: AssistantMessage,
}

#[derive(Debug, Deserialize)]
struct AssistantMessage {
    #[allow(dead_code)]
    role: Option<String>,
    content: Option<String>,

    // OpenAI tool-calling shape
    tool_calls: Option<Vec<ToolCallWire>>,
}

#[derive(Debug, Deserialize)]
struct ToolCallWire {
    id: String,
    #[serde(rename = "type")]
    _type: String,
    function: FunctionWire,
}

#[derive(Debug, Deserialize)]
struct FunctionWire {
    name: String,
    arguments: String, // JSON string per OpenAI spec
}

#[derive(Debug, Deserialize)]
struct UsageWire {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

impl ChatCompletionsResponse {
    fn into_completion(self) -> Completion {
        let mut outputs = Vec::new();

        if let Some(choice) = self.choices.into_iter().next() {
            if let Some(tool_calls) = choice.message.tool_calls {
                for tc in tool_calls {
                    let args: Json = serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(Json::Object(Default::default()));
                    outputs.push(LlmOutput::ToolCall(ToolCall {
                        id: tc.id,
                        name: tc.function.name,
                        args: args,
                    }));
                }
            }

            if let Some(content) = choice.message.content {
                let c = content.trim();
                if !c.is_empty() {
                    outputs.push(LlmOutput::AssistantText(c.to_string()));
                }
            }
        }

        Completion {
            outputs,
            usage: self.usage.map(|u| Usage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            }),
        }
    }
}
