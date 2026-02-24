use crabbot_shared::api::ui_html::{UiHtmlGetResp, UiHtmlUpdate};
use gloo_events::EventListener;
use gloo_net::http::Request;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use web_sys::{EventSource, MessageEvent};

use crabbot_shared::api::model::{
    ListSessionsResp, PostMessageReq, PostMessageResp, TranscriptResp,
};
use crabbot_shared::api::transcript::TranscriptEvent;

fn base_url(base_http: &str) -> String {
    base_http.trim_end_matches('/').to_string()
}

pub async fn api_list_sessions(base_http: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/v1/sessions", base_url(base_http));

    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, body));
    }

    let parsed: ListSessionsResp = resp.json().await.map_err(|e| e.to_string())?;

    Ok(parsed.session_keys)
}

pub async fn api_get_transcript(
    base_http: &str,
    session_key: &str,
    after_ts_ms: Option<i64>,
    limit: Option<usize>,
) -> Result<TranscriptResp, String> {
    let mut url = format!(
        "{}/v1/sessions/{}/transcript",
        base_url(base_http),
        urlencoding::encode(session_key),
    );

    let mut first = true;
    if let Some(v) = after_ts_ms {
        url.push(if first { '?' } else { '&' });
        first = false;
        url.push_str(&format!("after_ts_ms={}", v));
    }
    if let Some(v) = limit {
        url.push(if first { '?' } else { '&' });
        url.push_str(&format!("limit={}", v));
    }

    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, body));
    }

    resp.json().await.map_err(|e| e.to_string())
}

pub async fn api_post_message(
    base_http: &str,
    session_key: &str,
    text: String,
) -> Result<PostMessageResp, String> {
    let url = format!(
        "{}/v1/sessions/{}/message",
        base_url(base_http),
        urlencoding::encode(session_key),
    );

    let resp = Request::post(&url)
        .json(&PostMessageReq { text })
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, body));
    }

    resp.json().await.map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct TranscriptSse {
    es: EventSource,
    _on_open: EventListener,
    _on_error: EventListener,
    _on_message: EventListener,
    _on_transcript: EventListener,
    _on_session: EventListener,
}

impl TranscriptSse {
    pub fn close(&self) {
        self.es.close();
    }
}

pub fn api_stream_transcript(
    base_http: &str,
    session_key: &str,
    after_ts_ms: i64,
    on_event: impl FnMut(TranscriptEvent) + 'static,
    on_error: impl FnMut(String) + 'static,
) -> Result<TranscriptSse, String> {
    let url = format!(
        "{}/v1/sessions/{}/transcript/stream?after_ts_ms={}",
        base_url(base_http),
        urlencoding::encode(session_key),
        after_ts_ms
    );

    let es = EventSource::new(&url).map_err(|e| format!("EventSource open failed: {e:?}"))?;

    let on_event = Rc::new(RefCell::new(on_event));
    let on_error = Rc::new(RefCell::new(on_error));

    let on_open = EventListener::new(&es, "open", |_evt| {});

    let on_error_listener = {
        let on_error = on_error.clone();
        EventListener::new(&es, "error", move |_evt| {
            (on_error.borrow_mut())("sse error".into());
        })
    };

    // default SSE "message" (only if server sends unnamed events)
    let on_message = EventListener::new(&es, "message", |_evt| {});

    // named event: "transcript"
    let on_transcript = {
        let on_event = on_event.clone();
        let on_error = on_error.clone();
        EventListener::new(&es, "transcript", move |evt| {
            let me: MessageEvent = evt.dyn_ref::<MessageEvent>().unwrap().clone();
            let Some(s) = me.data().as_string() else {
                return;
            };

            match serde_json::from_str::<TranscriptEvent>(&s) {
                Ok(ev) => (on_event.borrow_mut())(ev),
                Err(e) => (on_error.borrow_mut())(format!("sse parse TranscriptEvent failed: {e}")),
            }
        })
    };

    let on_session = EventListener::new(&es, "session", |_evt| {});

    Ok(TranscriptSse {
        es,
        _on_open: on_open,
        _on_error: on_error_listener,
        _on_message: on_message,
        _on_transcript: on_transcript,
        _on_session: on_session,
    })
}

pub async fn api_get_ui_html(base_http: &str, session_key: &str) -> Result<UiHtmlGetResp, String> {
    let url = format!(
        "{}/v1/sessions/{}/ui_html",
        base_url(base_http),
        urlencoding::encode(session_key),
    );

    let resp = Request::get(&url).send().await.map_err(|e| e.to_string())?;

    if !resp.ok() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("HTTP {}: {}", status, body));
    }

    resp.json().await.map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct UiHtmlSse {
    es: EventSource,
    _on_open: EventListener,
    _on_error: EventListener,
    _on_message: EventListener,
    _on_ui_html: EventListener,
}

impl UiHtmlSse {
    pub fn close(&self) {
        self.es.close();
    }
}

/// `after_ts_ms` is used as Last-Event-ID by setting it in the URL as `after_ts_ms` if you want,
/// but your server currently reads `Last-Event-ID` header (EventSource can't set headers).
/// So: pass 0 and rely on server pushing only new events after connect; client refetches anyway.
/// If you keep the `Last-Event-ID` logic server-side, it won't work with EventSource headers.
pub fn api_stream_ui_html(
    base_http: &str,
    session_key: &str,
    on_update: impl FnMut(UiHtmlUpdate) + 'static,
    on_error: impl FnMut(String) + 'static,
) -> Result<UiHtmlSse, String> {
    let url = format!(
        "{}/v1/sessions/{}/ui_html/stream",
        base_url(base_http),
        urlencoding::encode(session_key),
    );

    let es = EventSource::new(&url).map_err(|e| format!("EventSource open failed: {e:?}"))?;

    let on_update = Rc::new(RefCell::new(on_update));
    let on_error = Rc::new(RefCell::new(on_error));

    let on_open = EventListener::new(&es, "open", |_evt| {});

    let on_error_listener = {
        let on_error = on_error.clone();
        EventListener::new(&es, "error", move |_evt| {
            (on_error.borrow_mut())("ui_html sse error".into());
        })
    };

    let on_message = EventListener::new(&es, "message", |_evt| {});

    let on_ui_html = {
        let on_update = on_update.clone();
        let on_error = on_error.clone();
        EventListener::new(&es, "ui_html", move |evt| {
            let me: MessageEvent = evt.dyn_ref::<MessageEvent>().unwrap().clone();
            let Some(s) = me.data().as_string() else {
                return;
            };

            match serde_json::from_str::<UiHtmlUpdate>(&s) {
                Ok(ev) => (on_update.borrow_mut())(ev),
                Err(e) => (on_error.borrow_mut())(format!("sse parse UiHtmlUpdate failed: {e}")),
            }
        })
    };

    Ok(UiHtmlSse {
        es,
        _on_open: on_open,
        _on_error: on_error_listener,
        _on_message: on_message,
        _on_ui_html: on_ui_html,
    })
}
