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
