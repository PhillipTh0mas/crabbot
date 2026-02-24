use crate::{
    agent::types::{AgentProfile, MdInclude},
    config::Paths,
    error::Result,
};
use crabbot_shared::api::transcript::TranscriptEvent;

/// Loads agent bootstrap markdown into transcript events.
pub async fn load_md_events(paths: &Paths, agent: &AgentProfile) -> Result<Vec<TranscriptEvent>> {
    let mut out = Vec::new();

    for inc in &agent.md_includes {
        match inc {
            MdInclude::Sould => {
                load_file_abs(paths.soul_file(), "SOUL.md", &mut out).await;
            }
        }
    }

    Ok(out)
}

async fn load_file_abs(abs: std::path::PathBuf, rel_label: &str, out: &mut Vec<TranscriptEvent>) {
    if let Ok(s) = tokio::fs::read_to_string(&abs).await {
        out.push(file_as_system_event(rel_label, &s));
    }
}

fn file_as_system_event(rel: &str, contents: &str) -> TranscriptEvent {
    let text = format!("Loaded file: {rel}\n\n{contents}");
    TranscriptEvent::custom_message("system", text)
}
