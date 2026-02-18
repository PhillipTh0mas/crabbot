use std::collections::HashMap;

use gloo_net::websocket::Message as WsMessage;

use crabbot_shared::api::transcript::TranscriptEvent;

#[derive(Debug, serde::Serialize)]
#[serde(tag = "type")]
pub enum WsIn {
    #[serde(rename = "send")]
    Send { text: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "type")]
pub enum WsOut {
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "response")]
    Response { session_id: String, text: String },
    #[serde(rename = "transcript")]
    Transcript { event: TranscriptEvent },
}

pub type SessionEventsMap = HashMap<String, Vec<TranscriptEvent>>;

pub type WsTx = futures_channel::mpsc::UnboundedSender<WsMessage>;
