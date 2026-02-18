use crabbot_shared::api::transcript::TranscriptEvent;
use serde_json::json;

use crate::{config::PromptConfig, storage::session_store::SessionEntry};

#[derive(Debug)]
pub struct DefaultPromptBuilder {
    cfg: PromptConfig,
}

impl DefaultPromptBuilder {
    pub fn new(cfg: PromptConfig) -> Self {
        Self { cfg }
    }

    pub fn is_valid_compaction(&self, summary: &str) -> bool {
        summary.chars().count() <= self.cfg.compaction_max_chars
    }

    pub fn trim_summary(&self, summary: &str) -> String {
        summary
            .chars()
            .take(self.cfg.compaction_max_chars)
            .collect::<String>()
    }

    pub fn get_max_chars(&self) -> usize {
        self.cfg.compaction_max_chars
    }

    pub fn build_compact_messages(
        &self,
        _session: &SessionEntry,
        compacted_events: &[TranscriptEvent],
    ) -> Vec<serde_json::Value> {
        // Invariant expected by caller:
        // - compacted_events is the prefix that will be replaced by a single CompactionSummary
        // - compacted_events may start with CompactionSummary (index 0) or not
        // - tool calling MUST NOT happen in compaction

        let max_chars = self.cfg.compaction_max_chars; // adjust to your config; or make it required

        let mut messages = Vec::new();

        messages.push(json!({
                "role": "system",
                "content": format!(
                    "You are a conversation compaction engine.\n\
                     Task: produce an UPDATED long-term memory summary.\n\
                     Output: ONLY the summary text (no JSON, no markdown, no quotes).\n\
                     Length: maximum {} characters.\n\
                     Include: stable facts, decisions, plans, open tasks, user preferences, and important tool outcomes (IDs/commands/errors).\n\
                     Exclude: filler, chit-chat, repetition, low-signal details.\n\
                     Do NOT propose actions. Do NOT call tools.",
                    max_chars
                )
            }));

        // If the first event is already a summary, treat it as "existing summary" to be updated.
        let (existing_summary, start_idx) = match compacted_events.first() {
            Some(TranscriptEvent::CompactionSummary(cs)) => (Some(cs.summary.as_str()), 1),
            _ => (None, 0),
        };

        let to_merge = &compacted_events[start_idx..];

        let mut input = String::new();

        if let Some(s) = existing_summary {
            let s = s.trim();
            if !s.is_empty() {
                input.push_str("Existing summary:\n");
                input.push_str(s);
                input.push_str("\n\n");
            }
        }

        input.push_str("New conversation to merge into the summary:\n");
        input.push_str(&TranscriptEvent::render_events_for_compaction(to_merge));

        messages.push(json!({
            "role": "user",
            "content": input
        }));

        messages
    }

    pub fn build_messages(
        &self,
        _session: &SessionEntry,
        events: &[TranscriptEvent],
    ) -> Vec<serde_json::Value> {
        let mut messages = Vec::new();

        messages.push(json!({
            "role": "system",
            "content": self.cfg.system_preamble
        }));

        messages.extend(TranscriptEvent::vec_to_prompt_messages(events));

        messages
    }
}
