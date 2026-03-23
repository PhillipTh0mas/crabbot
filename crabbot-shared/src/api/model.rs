use serde::{Deserialize, Serialize};

use crate::api::transcript::TranscriptEvent;

#[derive(Debug, Serialize, Deserialize)]
pub struct PostMessageReq {
    pub text: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PostMessageResp {
    pub ok: bool,
    pub session_id: String,
    pub response: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListSessionsResp {
    pub session_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub key: String,
    /// One of: "user", "thinking", "task", "tool", "unknown"
    pub session_type: String,
    /// Human-readable label (e.g. "Default Chat", "Background Thinking", task description, tool name)
    pub label: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListSessionsDetailedResp {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMemoryReq {
    /// Which memory to update: "short_term" or "daily"
    pub memory_type: String,
    /// The new content
    pub content: String,
    /// For daily memory, the date (YYYY-MM-DD). Ignored for short_term.
    #[serde(default)]
    pub daily_date: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryResp {
    pub short_term: String,
    pub daily: String,
    pub daily_date: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptQuery {
    #[serde(default)]
    pub after_ts_ms: Option<i64>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptResp {
    pub session_id: String,
    pub events: Vec<TranscriptEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InFlightSessionsResp {
    pub session_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionStatsResp {
    pub tool_name: String,
    pub total_calls: u64,
    pub total_errors: u64,
    pub entry_count: usize,
    pub compaction_count: u64,
    pub last_call_ts_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionsListResp {
    pub tools: Vec<ToolSessionStatsResp>,
}

/// A single entry in a tool's session history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionEntryResp {
    pub ts_ms: i64,
    pub kind: ToolSessionEntryKindResp,
}

/// The kind of tool session entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolSessionEntryKindResp {
    Call {
        call_id: String,
        args_summary: String,
    },
    Result {
        call_id: String,
        success: bool,
        output_summary: String,
    },
    Error {
        call_id: String,
        error: String,
    },
    CompactionSummary {
        summary: String,
        covers_up_to_ts_ms: i64,
        entries_compacted: usize,
    },
    Note {
        text: String,
    },
}

/// Response for GET /v1/tool_sessions/{tool_name}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSessionHistoryResp {
    pub tool_name: String,
    pub total_calls: u64,
    pub total_errors: u64,
    pub compaction_count: u64,
    pub last_call_ts_ms: Option<i64>,
    pub entries: Vec<ToolSessionEntryResp>,
}
