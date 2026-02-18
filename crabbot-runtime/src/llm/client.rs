use async_trait::async_trait;
use crabbot_shared::api::transcript::TranscriptEvent;

use crate::{error::Result, storage::session_store::SessionEntry, tools::tool::ToolSpec};

use super::types::Completion;

#[async_trait]
pub trait Llm: Send + Sync + std::fmt::Debug {
    /// Provider-specific request builder + completion.
    ///
    /// The caller provides canonical transcript/history; the LLM adapter is responsible for:
    /// - formatting history into provider-specific message/prompt structure
    /// - attaching tools in the provider's expected way (native tools vs prompt-contract)
    /// - returning a normalized Completion (assistant text and/or tool calls)
    async fn complete(
        &self,
        session: &SessionEntry,
        events: &[TranscriptEvent],
        tools: &[ToolSpec],
    ) -> Result<Completion>;

    /// Provider-specific compaction request.
    ///
    /// `compacted_events` is the prefix slice that will be replaced by a single CompactionSummary.
    /// It may start with an existing CompactionSummary at index 0.
    async fn compact(
        &self,
        session: &SessionEntry,
        compacted_events: &[TranscriptEvent],
    ) -> Result<String>;

    /// Optional: estimate prompt size for compaction policy (rough is fine).
    fn estimate_prompt_chars(
        &self,
        _session: &SessionEntry,
        events: &[TranscriptEvent],
        user_text: &str,
    ) -> usize {
        TranscriptEvent::estimate_prompt_chars(events, user_text)
    }
}
