use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::error::{Error, Result};
use crate::memory::service::MemoryService;
use crate::memory::types::{MemorySearchHit, MemorySearchQuery};
use crate::tools::tool::{Tool, ToolCall, ToolResult, ToolSpec};

fn default_max_chars() -> usize {
    8_000
}

#[derive(Debug)]
pub struct MemoryTool {
    memory: Arc<MemoryService>,
    max_return_chars_total: usize,
    max_return_chars_per_hit: usize,
}

impl MemoryTool {
    pub fn new(memory: Arc<MemoryService>) -> Self {
        Self {
            memory,
            max_return_chars_total: default_max_chars(),
            max_return_chars_per_hit: 2_000,
        }
    }

    pub fn with_max_return_chars_total(mut self, n: usize) -> Self {
        self.max_return_chars_total = n.max(1024);
        self
    }

    pub fn with_max_return_chars_per_hit(mut self, n: usize) -> Self {
        self.max_return_chars_per_hit = n.max(256);
        self
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum MemoryArgs {
    /// Add a chunk to the indexed memory store (you plan to call this "long term").
    Save {
        text: String,
    },

    /// Search the indexed memory store.
    Find {
        query: String,
        #[serde(default)]
        top_k: Option<usize>,
    },

    /// Read the current short-term memory document (currently backed by MemoryService::get_long_term()).
    GetShortTerm {},

    /// Replace the short-term memory document (currently backed by MemoryService::write_long_term_replace()).
    ReplaceShortTerm {
        text: String,
    },

    AppendShortTerm {
        text: String,
    },
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
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

fn append_with_budget(dst: &mut String, s: &str, remaining: &mut usize) {
    if *remaining == 0 {
        return;
    }
    let chunk = truncate_chars(s, *remaining);
    *remaining = remaining.saturating_sub(chunk.chars().count());
    dst.push_str(&chunk);
}

fn format_hits_plain(
    hits: Vec<MemorySearchHit>,
    max_total_chars: usize,
    max_per_hit: usize,
) -> String {
    if hits.is_empty() {
        return "No memory hits.".to_string();
    }

    let mut out = String::new();
    let mut remaining = max_total_chars;

    for h in hits {
        if remaining == 0 {
            break;
        }

        let body = truncate_chars(&h.text, max_per_hit);
        append_with_budget(&mut out, &body, &mut remaining);
        append_with_budget(&mut out, "\n\n", &mut remaining);
    }

    if out.trim().is_empty() {
        "No memory hits.".to_string()
    } else {
        out.trim_end().to_string()
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: "Memory tool: (1) indexed memory (save/find) and (2) short-term memory document (get/replace). Returns plain text suitable for direct injection into an agent prompt."
                .to_string(),
            parameters: json!({
                "type": "object",
                "oneOf": [
                    {
                        "type": "object",
                        "required": ["op", "text"],
                        "properties": {
                            "op": { "const": "save" },
                            "text": { "type": "string" }
                        },
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "required": ["op", "query"],
                        "properties": {
                            "op": { "const": "find" },
                            "query": { "type": "string" },
                            "top_k": { "type": "integer", "minimum": 1, "default": 8 }
                        },
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "required": ["op"],
                        "properties": {
                            "op": { "const": "get_short_term" }
                        },
                        "additionalProperties": false
                    },
                    {
                        "type": "object",
                        "required": ["op", "text"],
                        "properties": {
                            "op": { "const": "replace_short_term" },
                            "text": { "type": "string" }
                        },
                        "additionalProperties": false
                    }
                ]
            }),
        }
    }

    async fn call(&self, call: ToolCall, _session_key: &str) -> Result<ToolResult> {
        let args: MemoryArgs = serde_json::from_value(call.args)
            .map_err(|e| Error::bad_request(format!("memory tool: invalid args schema: {e}")))?;

        let text_out = match args {
            MemoryArgs::Save { text } => {
                let source = uuid::Uuid::new_v4().to_string();
                match self.memory.save_to_index(&text, &source).await {
                    Ok(()) => "Successfully saved to index.".to_string(),
                    Err(e) => {
                        tracing::warn!(
                            "memory.save_to_index failed (embedding service may be unavailable): {e}"
                        );
                        format!(
                            "Failed to save to indexed memory (embedding service unavailable). The text was not indexed. Error: {e}"
                        )
                    }
                }
            }

            MemoryArgs::Find { query, top_k } => {
                let hits = match self
                    .memory
                    .search_index(MemorySearchQuery {
                        query,
                        top_k: top_k.unwrap_or(0),
                        kind: None,
                        date_from: None,
                        date_to: None,
                    })
                    .await
                {
                    Ok(hits) => hits,
                    Err(e) => {
                        tracing::warn!(
                            "memory.search_index failed (embedding service may be unavailable): {e}"
                        );
                        vec![]
                    }
                };

                format_hits_plain(
                    hits,
                    self.max_return_chars_total,
                    self.max_return_chars_per_hit,
                )
            }

            MemoryArgs::GetShortTerm {} => {
                // Currently stored in "long term" file APIs, but you’re renaming it to "short term".
                // Swap to self.memory.get_short_term() once you rename.
                let s = self.memory.get_short_term().await?;
                if s.trim().is_empty() {
                    "Short-term memory is empty.".to_string()
                } else {
                    truncate_chars(&s, self.max_return_chars_total)
                }
            }

            MemoryArgs::AppendShortTerm { text } => {
                // Currently stored in "long term" file APIs, but you’re renaming it to "short term".
                // Swap to self.memory.write_short_term_append(&text) once you rename.
                let old_content = self.memory.get_short_term().await?;
                let appended = format!("{}/n{}", old_content, text);
                self.memory.update_short_term(&appended).await?;
                format!("Successfully appended {} to short-term memory", &text)
            }

            MemoryArgs::ReplaceShortTerm { text } => {
                // Currently stored in "long term" file APIs, but you’re renaming it to "short term".
                // Swap to self.memory.write_short_term_replace(&text) once you rename.
                self.memory.update_short_term(&text).await?;
                let new_content = self.memory.get_short_term().await?;
                format!(
                    "Successfully replaced short-term memory with: {}",
                    new_content
                )
            }
        };

        Ok(ToolResult::ok(call.id, call.name, json!(text_out)))
    }
}
