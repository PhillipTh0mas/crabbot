use crate::{
    error::Result,
    llm::{client::Llm, types::Completion},
    memory::{service::MemoryService, types::FlushResult},
    storage::session_store::SessionEntry,
};
use crabbot_shared::api::transcript::TranscriptEvent;
use serde::Deserialize;

pub fn build_flush_instruction() -> String {
    "Extract durable memories from the conversation and output ONLY valid JSON with this schema:\n\
     {\"daily\":[\"...\"],\"long_term\":[\"...\"]}\n\
     Rules:\n\
     - daily: short-lived facts relevant for today (tasks, plans, immediate context)\n\
     - long_term: stable preferences, identity facts, recurring projects\n\
     - do NOT include sensitive data unless user explicitly requested it\n\
     - keep each item short (<= 200 chars)\n\
     - if nothing to store, return {\"daily\":[],\"long_term\":[]}"
        .to_string()
}

pub fn render_for_memory_flush(events: &[TranscriptEvent], max_chars: usize) -> String {
    let s = TranscriptEvent::render_events_for_compaction(events);
    truncate_chars(&s, max_chars)
}

#[derive(Debug, Deserialize)]
struct FlushJson {
    daily: Vec<String>,
    long_term: Vec<String>,
}

fn extract_first_text(c: &Completion) -> Option<String> {
    for o in &c.outputs {
        if let crate::llm::types::LlmOutput::AssistantText(t) = o {
            let s = t.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

pub fn parse_flush_json(s: &str) -> FlushResult {
    let parsed: FlushJson = serde_json::from_str(s).unwrap_or(FlushJson {
        daily: vec![],
        long_term: vec![],
    });

    FlushResult {
        daily: parsed
            .daily
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        long_term: parsed
            .long_term
            .into_iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
    }
}

/// Provider-agnostic flush call: engine-owned policy, but mechanics here.
/// No tools. Returns parsed items; caller decides how/when to persist.
pub async fn run_memory_flush(
    llm: &dyn Llm,
    session: &SessionEntry,
    tail_for_prompt: &[TranscriptEvent],
    flush_context_max_chars: usize,
) -> Result<FlushResult> {
    tracing::info!("Running memory flush for session {}", session.session_key);
    let ctx = render_for_memory_flush(tail_for_prompt, flush_context_max_chars);

    let events = vec![
        TranscriptEvent::custom_message("system", build_flush_instruction()),
        TranscriptEvent::custom_message("user", ctx),
    ];

    let completion = llm.complete(session, &events, &[]).await?;

    let json_text = extract_first_text(&completion)
        .unwrap_or_else(|| "{\"daily\":[],\"long_term\":[]}".to_string());

    Ok(parse_flush_json(&json_text))
}

/// “Default write policy”: append daily items to today's file;
/// long_term items appended as bullet points to MEMORY.md.
/// If you later want promotion/merging, change it here.
pub async fn apply_flush_result(
    memory: &MemoryService,
    local_day: &str,
    res: FlushResult,
) -> Result<()> {
    for item in res.daily {
        memory.write_daily_append(local_day, &item).await?;
    }

    if !res.long_term.is_empty() {
        let existing = memory.get_short_term().await.unwrap_or_default();
        let mut merged = existing;
        if !merged.ends_with('\n') && !merged.is_empty() {
            merged.push('\n');
        }
        for item in res.long_term {
            merged.push_str("- ");
            merged.push_str(item.trim());
            merged.push('\n');
        }
        memory.write_short_term_replace(&merged).await?;
    }

    Ok(())
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
