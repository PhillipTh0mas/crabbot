// crabbot-runtime/src/tools/tool_sessions.rs
//
// Per-tool persistent sessions.
//
// Each tool gets its own session that accumulates context across all calls.
// This lets the agent build up "institutional knowledge" about how to use
// each tool correctly (common patterns, past errors, effective arg combos).
//
// Each tool session has its own compaction (independent of any user session)
// and does NOT share memory with other tool sessions or user sessions.

use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::{Error, Result};
use crate::tools::tool::ToolCall;

/// Maximum number of entries in a tool session before compaction fires.
const DEFAULT_COMPACTION_THRESHOLD: usize = 80;

/// Maximum character budget for the compacted summary.
const DEFAULT_SUMMARY_MAX_CHARS: usize = 4_000;

/// Maximum number of recent entries to keep after compaction
/// (the "tail" that stays uncompacted for recency).
const DEFAULT_KEEP_RECENT: usize = 10;

// ─── Types ───────────────────────────────────────────────────────────────────

/// A single entry in a tool's session log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionEntry {
    pub ts_ms: i64,
    pub kind: ToolSessionEntryKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolSessionEntryKind {
    /// A tool was called with these args.
    Call {
        call_id: String,
        args_summary: String,
    },
    /// The tool returned a result.
    Result {
        call_id: String,
        success: bool,
        output_summary: String,
    },
    /// An error occurred during the tool call.
    Error { call_id: String, error: String },
    /// A compaction summary replacing older entries.
    CompactionSummary {
        summary: String,
        covers_up_to_ts_ms: i64,
        entries_compacted: usize,
    },
    /// Arbitrary note (e.g. usage hints discovered by the agent).
    Note { text: String },
}

/// Persistent state for one tool's session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionState {
    pub tool_name: String,
    pub entries: Vec<ToolSessionEntry>,
    pub total_calls: u64,
    pub total_errors: u64,
    pub last_call_ts_ms: Option<i64>,
    pub compaction_count: u64,
}

impl ToolSessionState {
    pub fn new(tool_name: &str) -> Self {
        Self {
            tool_name: tool_name.to_string(),
            entries: Vec::new(),
            total_calls: 0,
            total_errors: 0,
            last_call_ts_ms: None,
            compaction_count: 0,
        }
    }

    /// Push an entry into the session log.
    pub fn push(&mut self, entry: ToolSessionEntry) {
        self.entries.push(entry);
    }

    /// Record a tool call.
    pub fn record_call(&mut self, call: &ToolCall) {
        let now = crate::time::now_ts_ms();
        self.total_calls += 1;
        self.last_call_ts_ms = Some(now);

        // Summarize args to avoid storing huge payloads.
        let args_summary = summarize_json(&call.args, 500);

        self.push(ToolSessionEntry {
            ts_ms: now,
            kind: ToolSessionEntryKind::Call {
                call_id: call.id.clone(),
                args_summary,
            },
        });
    }

    /// Record a successful tool result.
    pub fn record_result(&mut self, call_id: &str, output: &serde_json::Value) {
        let now = crate::time::now_ts_ms();
        let output_summary = summarize_json(output, 500);

        self.push(ToolSessionEntry {
            ts_ms: now,
            kind: ToolSessionEntryKind::Result {
                call_id: call_id.to_string(),
                success: true,
                output_summary,
            },
        });
    }

    /// Record a tool error.
    pub fn record_error(&mut self, call_id: &str, error: &str) {
        let now = crate::time::now_ts_ms();
        self.total_errors += 1;

        self.push(ToolSessionEntry {
            ts_ms: now,
            kind: ToolSessionEntryKind::Error {
                call_id: call_id.to_string(),
                error: truncate_str(error, 500),
            },
        });
    }

    /// Record an arbitrary note.
    pub fn record_note(&mut self, text: &str) {
        let now = crate::time::now_ts_ms();
        self.push(ToolSessionEntry {
            ts_ms: now,
            kind: ToolSessionEntryKind::Note {
                text: truncate_str(text, 1000),
            },
        });
    }

    /// Returns true if the session log is large enough to warrant compaction.
    pub fn needs_compaction(&self, threshold: usize) -> bool {
        self.entries.len() > threshold
    }

    /// Build a plain-text rendering of the session for injection into a prompt.
    /// This is what gets prepended when the tool is about to be called so the
    /// LLM has context about past usage.
    pub fn render_context(&self, max_chars: usize) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        let mut buf = format!(
            "[Tool session for '{}': {} total calls, {} errors]\n",
            self.tool_name, self.total_calls, self.total_errors,
        );

        // Render entries newest-first but cap at budget.
        for entry in self.entries.iter().rev() {
            let line = render_entry(entry);
            if buf.len() + line.len() + 1 > max_chars {
                buf.push_str("\n[...older entries omitted...]\n");
                break;
            }
            // We're iterating in reverse but we'll accept reverse-chronological
            // for the context window — most recent info is most valuable.
            buf.push_str(&line);
            buf.push('\n');
        }

        buf
    }

    /// Perform local (non-LLM) compaction: keep a summary of old entries
    /// and only the N most recent entries verbatim.
    ///
    /// For LLM-based compaction, the caller should use `render_context` to
    /// get the text, send it to the LLM for summarization, then call
    /// `apply_compaction_summary`.
    pub fn compact_local(&mut self, keep_recent: usize) {
        if self.entries.len() <= keep_recent {
            return;
        }

        let split_at = self.entries.len() - keep_recent;
        let old_entries: Vec<_> = self.entries.drain(..split_at).collect();

        if old_entries.is_empty() {
            return;
        }

        let covers_up_to_ts_ms = old_entries.last().map(|e| e.ts_ms).unwrap_or(0);
        let entries_compacted = old_entries.len();

        // Build a simple statistical summary of the compacted entries.
        let mut calls = 0usize;
        let mut errors = 0usize;
        let mut results = 0usize;
        let mut error_snippets: Vec<String> = Vec::new();

        for e in &old_entries {
            match &e.kind {
                ToolSessionEntryKind::Call { .. } => calls += 1,
                ToolSessionEntryKind::Result { success, .. } => {
                    results += 1;
                    if !success {
                        errors += 1;
                    }
                }
                ToolSessionEntryKind::Error { error, .. } => {
                    errors += 1;
                    if error_snippets.len() < 3 {
                        error_snippets.push(truncate_str(error, 200));
                    }
                }
                ToolSessionEntryKind::CompactionSummary { summary, .. } => {
                    // Carry forward the previous compaction summary.
                    if error_snippets.is_empty() {
                        // Preserve a fragment of the old summary.
                        let frag = truncate_str(summary, 500);
                        error_snippets.push(format!("[prior summary]: {frag}"));
                    }
                }
                ToolSessionEntryKind::Note { text } => {
                    if error_snippets.len() < 5 {
                        error_snippets.push(format!("[note]: {}", truncate_str(text, 150)));
                    }
                }
            }
        }

        let mut summary = format!(
            "Compacted {entries_compacted} entries: {calls} calls, {results} results, {errors} errors."
        );

        if !error_snippets.is_empty() {
            summary.push_str("\nNotable items:\n");
            for s in &error_snippets {
                summary.push_str("- ");
                summary.push_str(s);
                summary.push('\n');
            }
        }

        let summary = truncate_str(&summary, DEFAULT_SUMMARY_MAX_CHARS);

        // Prepend the compaction summary.
        self.entries.insert(
            0,
            ToolSessionEntry {
                ts_ms: crate::time::now_ts_ms(),
                kind: ToolSessionEntryKind::CompactionSummary {
                    summary,
                    covers_up_to_ts_ms,
                    entries_compacted,
                },
            },
        );

        self.compaction_count += 1;
    }

    /// Apply an LLM-generated compaction summary, replacing all entries
    /// older than `keep_recent` with the summary.
    pub fn apply_compaction_summary(&mut self, summary: String, keep_recent: usize) {
        if self.entries.len() <= keep_recent {
            return;
        }

        let split_at = self.entries.len() - keep_recent;
        let old_entries: Vec<_> = self.entries.drain(..split_at).collect();
        let covers_up_to_ts_ms = old_entries.last().map(|e| e.ts_ms).unwrap_or(0);
        let entries_compacted = old_entries.len();

        self.entries.insert(
            0,
            ToolSessionEntry {
                ts_ms: crate::time::now_ts_ms(),
                kind: ToolSessionEntryKind::CompactionSummary {
                    summary: truncate_str(&summary, DEFAULT_SUMMARY_MAX_CHARS),
                    covers_up_to_ts_ms,
                    entries_compacted,
                },
            },
        );

        self.compaction_count += 1;
    }
}

// ─── Store ───────────────────────────────────────────────────────────────────

/// On-disk index of all tool sessions.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ToolSessionIndex {
    sessions: HashMap<String, ToolSessionState>,
}

/// Thread-safe store for all tool sessions.
/// Persists to a single JSON file.
#[derive(Debug)]
pub struct ToolSessionStore {
    path: PathBuf,
    index: RwLock<ToolSessionIndex>,
    compaction_threshold: usize,
    keep_recent: usize,
}

impl ToolSessionStore {
    /// Open (or create) the tool sessions store.
    pub async fn open(dir: PathBuf) -> Result<Self> {
        let path = dir.join("tool_sessions.json");

        let index = if path.exists() {
            let data = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| Error::io(format!("read tool_sessions.json: {e}")))?;
            serde_json::from_str(&data).unwrap_or_default()
        } else {
            ToolSessionIndex::default()
        };

        Ok(Self {
            path,
            index: RwLock::new(index),
            compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
            keep_recent: DEFAULT_KEEP_RECENT,
        })
    }

    /// Get or create a tool session by tool name.
    pub async fn get_or_create(&self, tool_name: &str) -> ToolSessionState {
        let idx = self.index.read().await;
        if let Some(s) = idx.sessions.get(tool_name) {
            return s.clone();
        }
        drop(idx);

        let mut idx = self.index.write().await;
        idx.sessions
            .entry(tool_name.to_string())
            .or_insert_with(|| ToolSessionState::new(tool_name))
            .clone()
    }

    /// Record a tool call and persist.
    pub async fn record_call(&self, tool_name: &str, call: &ToolCall) -> Result<()> {
        {
            let mut idx = self.index.write().await;
            let session = idx
                .sessions
                .entry(tool_name.to_string())
                .or_insert_with(|| ToolSessionState::new(tool_name));
            session.record_call(call);

            // Auto-compact if needed.
            if session.needs_compaction(self.compaction_threshold) {
                session.compact_local(self.keep_recent);
            }
        }
        self.save().await
    }

    /// Record a tool result and persist.
    pub async fn record_result(
        &self,
        tool_name: &str,
        call_id: &str,
        output: &serde_json::Value,
    ) -> Result<()> {
        {
            let mut idx = self.index.write().await;
            let session = idx
                .sessions
                .entry(tool_name.to_string())
                .or_insert_with(|| ToolSessionState::new(tool_name));
            session.record_result(call_id, output);
        }
        self.save().await
    }

    /// Record a tool error and persist.
    pub async fn record_error(&self, tool_name: &str, call_id: &str, error: &str) -> Result<()> {
        {
            let mut idx = self.index.write().await;
            let session = idx
                .sessions
                .entry(tool_name.to_string())
                .or_insert_with(|| ToolSessionState::new(tool_name));
            session.record_error(call_id, error);
        }
        self.save().await
    }

    /// Record an arbitrary note for a tool.
    pub async fn record_note(&self, tool_name: &str, text: &str) -> Result<()> {
        {
            let mut idx = self.index.write().await;
            let session = idx
                .sessions
                .entry(tool_name.to_string())
                .or_insert_with(|| ToolSessionState::new(tool_name));
            session.record_note(text);
        }
        self.save().await
    }

    /// Get the context string for a tool, suitable for prompt injection.
    /// Returns empty string if no session exists or it's empty.
    pub async fn get_tool_context(&self, tool_name: &str, max_chars: usize) -> String {
        let idx = self.index.read().await;
        match idx.sessions.get(tool_name) {
            Some(session) => session.render_context(max_chars),
            None => String::new(),
        }
    }

    /// Get context for multiple tools at once (batch lookup).
    pub async fn get_tool_contexts(
        &self,
        tool_names: &[&str],
        max_chars_per_tool: usize,
    ) -> HashMap<String, String> {
        let idx = self.index.read().await;
        let mut out = HashMap::new();
        for &name in tool_names {
            if let Some(session) = idx.sessions.get(name) {
                let ctx = session.render_context(max_chars_per_tool);
                if !ctx.is_empty() {
                    out.insert(name.to_string(), ctx);
                }
            }
        }
        out
    }

    /// List all tool names that have sessions.
    pub async fn list_tool_names(&self) -> Vec<String> {
        let idx = self.index.read().await;
        idx.sessions.keys().cloned().collect()
    }

    /// Get stats for a tool session.
    pub async fn get_stats(&self, tool_name: &str) -> Option<ToolSessionStats> {
        let idx = self.index.read().await;
        idx.sessions.get(tool_name).map(|s| ToolSessionStats {
            tool_name: s.tool_name.clone(),
            total_calls: s.total_calls,
            total_errors: s.total_errors,
            entry_count: s.entries.len(),
            compaction_count: s.compaction_count,
            last_call_ts_ms: s.last_call_ts_ms,
        })
    }

    /// Get the full session state for a tool (entries + stats).
    pub async fn get_session(&self, tool_name: &str) -> Option<ToolSessionState> {
        let idx = self.index.read().await;
        idx.sessions.get(tool_name).cloned()
    }

    /// Persist to disk (atomic write).
    async fn save(&self) -> Result<()> {
        let snapshot = { self.index.read().await.clone() };

        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(|e| Error::io(e.to_string()))?;

        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| Error::io(format!("create_dir_all: {e}")))?;
        }

        let tmp = self.path.with_extension("tmp");
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(|e| Error::io(format!("write tool_sessions.tmp: {e}")))?;
        tokio::fs::rename(&tmp, &self.path)
            .await
            .map_err(|e| Error::io(format!("rename tool_sessions.tmp: {e}")))?;

        Ok(())
    }
}

// ─── Public stats type ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct ToolSessionStats {
    pub tool_name: String,
    pub total_calls: u64,
    pub total_errors: u64,
    pub entry_count: usize,
    pub compaction_count: u64,
    pub last_call_ts_ms: Option<i64>,
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn render_entry(entry: &ToolSessionEntry) -> String {
    match &entry.kind {
        ToolSessionEntryKind::Call {
            call_id,
            args_summary,
        } => {
            format!("[call {}] args: {}", call_id, args_summary)
        }
        ToolSessionEntryKind::Result {
            call_id,
            success,
            output_summary,
        } => {
            let status = if *success { "ok" } else { "FAIL" };
            format!("[result {} {}] {}", call_id, status, output_summary)
        }
        ToolSessionEntryKind::Error { call_id, error } => {
            format!("[ERROR {}] {}", call_id, error)
        }
        ToolSessionEntryKind::CompactionSummary {
            summary,
            entries_compacted,
            ..
        } => {
            format!("[compacted {} entries] {}", entries_compacted, summary)
        }
        ToolSessionEntryKind::Note { text } => {
            format!("[note] {}", text)
        }
    }
}

fn summarize_json(val: &serde_json::Value, max_chars: usize) -> String {
    let s = match serde_json::to_string(val) {
        Ok(s) => s,
        Err(_) => format!("{:?}", val),
    };
    truncate_str(&s, max_chars)
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

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            args,
        }
    }

    #[test]
    fn test_record_and_render() {
        let mut session = ToolSessionState::new("command");

        let call = make_call("c1", "command", json!({"program": "ls", "args": ["-la"]}));
        session.record_call(&call);
        session.record_result("c1", &json!({"stdout": "file1\nfile2"}));

        let call2 = make_call(
            "c2",
            "command",
            json!({"program": "cat", "args": ["/etc/nope"]}),
        );
        session.record_call(&call2);
        session.record_error("c2", "file not found: /etc/nope");

        assert_eq!(session.total_calls, 2);
        assert_eq!(session.total_errors, 1);
        assert_eq!(session.entries.len(), 4);

        let ctx = session.render_context(5000);
        assert!(ctx.contains("command"));
        assert!(ctx.contains("2 total calls"));
        assert!(ctx.contains("1 errors"));
    }

    #[test]
    fn test_compaction() {
        let mut session = ToolSessionState::new("memory");

        // Add enough entries to trigger compaction.
        for i in 0..20 {
            let call = make_call(
                &format!("c{i}"),
                "memory",
                json!({"op": "find", "query": "test"}),
            );
            session.record_call(&call);
            session.record_result(&format!("c{i}"), &json!("ok"));
        }

        assert_eq!(session.entries.len(), 40);

        session.compact_local(5);

        // Should have: 1 compaction summary + 5 recent entries = 6
        assert_eq!(session.entries.len(), 6);
        assert_eq!(session.compaction_count, 1);

        // First entry should be the compaction summary.
        match &session.entries[0].kind {
            ToolSessionEntryKind::CompactionSummary {
                entries_compacted, ..
            } => {
                assert_eq!(*entries_compacted, 35);
            }
            other => panic!("expected CompactionSummary, got {:?}", other),
        }
    }

    #[test]
    fn test_render_context_budget() {
        let mut session = ToolSessionState::new("test");
        for i in 0..100 {
            session.record_note(&format!(
                "Note number {i} with some padding text to fill space"
            ));
        }

        let ctx = session.render_context(500);
        assert!(ctx.len() <= 600); // some slack for the header line
        assert!(ctx.contains("[...older entries omitted...]"));
    }

    #[test]
    fn test_truncate_str() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello…");
        assert_eq!(truncate_str("", 5), "");
    }
}
