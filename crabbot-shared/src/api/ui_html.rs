use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UiHtmlUpdate {
    Saved { ts_ms: i64, bytes: usize },
    Deleted { ts_ms: i64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiHtmlGetResp {
    pub exists: bool,
    pub html: String,
}
