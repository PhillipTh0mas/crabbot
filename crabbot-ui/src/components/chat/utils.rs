use crabbot_shared::api::transcript::TranscriptEvent;

// Adjust to match your real HTML variant.
pub fn event_html(ev: &TranscriptEvent) -> Option<String> {
    match ev {
        // TranscriptEvent::Html { html, .. } => Some(html.clone()),
        TranscriptEvent::UserFacingHtml(e) => Some(e.html.clone()),
        _ => None,
    }
}

pub fn latest_html(events: &[TranscriptEvent]) -> Option<String> {
    events.iter().rev().find_map(event_html)
}
