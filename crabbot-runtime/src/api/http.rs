use crate::{
    api::state::{AppState, HttpStateInner},
    config::ApiConfig,
    error::{Error, Result},
    run::RunEngine,
};
use axum::{
    Json, Router,
    extract::{
        Path, Query, State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, get_service, post},
};
use crabbot_shared::api::{
    model::{
        InFlightSessionsResp, ListSessionsDetailedResp, ListSessionsResp, MemoryResp,
        PostMessageReq, PostMessageResp, ToolSessionHistoryResp, ToolSessionsListResp,
        TranscriptQuery, TranscriptResp, UpdateMemoryReq,
    },
    transcript::TranscriptEvent,
    ui_html::{UiHtmlGetResp, UiHtmlUpdate},
};
use futures_util::{SinkExt, StreamExt as FuturesStreamExt}; // for WebSocket recv/send loops
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::convert::Infallible;
use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};
use tokio_stream::StreamExt as TokioStreamExt; // for BroadcastStream + tokio streams
use tokio_stream::wrappers::BroadcastStream;
use tower_http::services::ServeDir;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};

fn ui_dist_dir() -> PathBuf {
    env::var_os("CRABBOT_UI_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from("/home/phillip/projects/crabbot/target/debug/bundle/ui-dist")
        })
    // .unwrap_or_else(|| PathBuf::from("./ui-dist"))
}

pub fn build_router(engine: Arc<RunEngine>, cfg: ApiConfig) -> Result<Router> {
    let state = AppState {
        http: Arc::new(HttpStateInner { engine, cfg }),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let ui = get_service(ServeDir::new(ui_dist_dir()).append_index_html_on_directories(true));
    Ok(Router::new()
        .route("/health", get(health))
        // Sessions
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/detailed", get(list_sessions_detailed))
        .route("/v1/sessions/in_flight", get(get_in_flight_sessions))
        .route("/v1/tool_sessions", get(get_tool_sessions))
        .route(
            "/v1/tool_sessions/{tool_name}",
            get(get_tool_session_history),
        )
        .route("/v1/memory", get(get_memory).put(update_memory))
        .route("/v1/sessions/{session_key}/message", post(post_message))
        .route(
            "/v1/sessions/{session_key}/transcript",
            get(get_transcript_once),
        )
        .route(
            "/v1/sessions/{session_key}/transcript/stream",
            get(transcript_stream),
        )
        .route("/v1/sessions/{session_key}/ws", get(chat_ws))
        .route("/v1/sessions/{session_key}/ui_html", get(get_ui_html))
        .route(
            "/v1/sessions/{session_key}/ui_html/stream",
            get(ui_html_stream),
        )
        // Hooks
        .route("/hooks/wake", post(hooks_wake))
        .route("/hooks/agent", post(hooks_agent))
        .route("/hooks/{name}", post(hooks_named))
        // Web UI at /
        .nest_service("/ui", ui)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(cors))
}

pub async fn serve(app: Router, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

// ---------------- auth ----------------

fn authorize_gateway(cfg: &ApiConfig, headers: &HeaderMap) -> Result<()> {
    // let got = headers
    //     .get(header::AUTHORIZATION)
    //     .ok_or_else(|| Error::unauthorized("missing Authorization header"))?
    //     .to_str()
    //     .map_err(|_| Error::unauthorized("invalid Authorization header"))?;

    // let want = format!("Bearer {}", cfg.auth_token);
    // if got != want {
    //     return Err(Error::unauthorized("invalid token"));
    // }
    Ok(())
}

fn authorize_hooks(cfg: &ApiConfig, headers: &HeaderMap) -> Result<()> {
    if let Some(hv) = headers.get(header::AUTHORIZATION) {
        let s = hv
            .to_str()
            .map_err(|_| Error::unauthorized("invalid Authorization header"))?;
        let want = format!("Bearer {}", cfg.auth_token);
        if s == want {
            return Ok(());
        }
    }
    if let Some(hv) = headers.get("x-openclaw-token") {
        let s = hv
            .to_str()
            .map_err(|_| Error::unauthorized("invalid x-openclaw-token header"))?;
        if s == cfg.auth_token {
            return Ok(());
        }
    }
    Err(Error::unauthorized("missing/invalid hook token"))
}

fn json_err(msg: &str) -> Value {
    json!({ "error": msg })
}

// ---------------- sessions: list session keys ----------------

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    let session_keys = state.http.engine.list_session_keys().await?;
    Ok((StatusCode::OK, Json(ListSessionsResp { session_keys })))
}

async fn list_sessions_detailed(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    let sessions = state.http.engine.list_sessions_detailed().await?;
    Ok((StatusCode::OK, Json(ListSessionsDetailedResp { sessions })))
}

async fn get_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    let (short_term, daily, daily_date) = state.http.engine.get_memory_snapshot().await?;
    Ok((
        StatusCode::OK,
        Json(MemoryResp {
            short_term,
            daily,
            daily_date,
        }),
    ))
}

async fn get_in_flight_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    let session_keys = state.http.engine.get_in_flight_sessions().await?;
    Ok((StatusCode::OK, Json(InFlightSessionsResp { session_keys })))
}

async fn get_tool_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    let tools = state.http.engine.get_tool_sessions_stats().await?;
    Ok((StatusCode::OK, Json(ToolSessionsListResp { tools })))
}

async fn get_tool_session_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(tool_name): Path<String>,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;
    match state
        .http
        .engine
        .get_tool_session_history(&tool_name)
        .await?
    {
        Some(history) => Ok((StatusCode::OK, Json(history)).into_response()),
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(json_err(&format!("tool session '{}' not found", tool_name))),
        )
            .into_response()),
    }
}

async fn update_memory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<UpdateMemoryReq>,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;

    match req.memory_type.as_str() {
        "short_term" => {
            state
                .http
                .engine
                .memory
                .write_short_term_replace(&req.content)
                .await
                .map_err(|e| Error::other(format!("Failed to update short-term memory: {e}")))?;
        }
        "daily" => {
            let date = req
                .daily_date
                .unwrap_or_else(|| crate::time::local_day_string());
            state
                .http
                .engine
                .memory
                .update_daily_replace(&date, &req.content)
                .await
                .map_err(|e| Error::other(format!("Failed to update daily memory: {e}")))?;
        }
        other => {
            return Err(Error::bad_request(format!(
                "Unknown memory_type: {other}. Use 'short_term' or 'daily'."
            )));
        }
    }

    Ok((StatusCode::OK, Json(serde_json::json!({"ok": true}))))
}

// ---------------- chat: POST message ----------------

async fn post_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
    Json(req): Json<PostMessageReq>,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;

    if req.text.trim().is_empty() {
        return Ok((StatusCode::BAD_REQUEST, Json(json_err("text is required"))).into_response());
    }

    let reply = state
        .http
        .engine
        .handle_message(session_key, req.text)
        .await?;

    Ok((
        StatusCode::OK,
        Json(PostMessageResp {
            ok: true,
            session_id: reply.session_id,
            response: reply.response,
        }),
    )
        .into_response())
}

async fn get_transcript_once(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;

    // Recommended engine API:
    // let (session_id, events) = engine.get_transcript(&session_key, q.after_ts_ms, q.limit).await?;
    //
    // If you only have get_transcript(session_key)->Vec<...>, do filtering here.
    let (session_id, mut events) = state
        .http
        .engine
        .get_transcript(&session_key, q.after_ts_ms, q.limit)
        .await?;

    if let Some(after) = q.after_ts_ms {
        events.retain(|e| e.ts_ms() > after);
    }
    if let Some(limit) = q.limit {
        if events.len() > limit {
            events = events.split_off(events.len() - limit);
        }
    }

    Ok((StatusCode::OK, Json(TranscriptResp { session_id, events })))
}

async fn transcript_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
    Query(q): Query<TranscriptQuery>,
) -> Result<Response> {
    authorize_gateway(&state.http.cfg, &headers)?;

    let after_ts_ms: i64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .or(q.after_ts_ms)
        .unwrap_or(0);

    // Subscribe first to avoid missing events between replay and live.
    let (session_id, rx) = state.http.engine.subscribe_transcript(&session_key).await?;

    // Replay from storage
    let (_sid2, mut events) = state
        .http
        .engine
        .get_transcript(&session_key, Some(after_ts_ms), q.limit)
        .await?;
    events.retain(|e| e.ts_ms() > after_ts_ms);

    let sid_event = tokio_stream::iter([Ok::<Event, Infallible>(
        Event::default()
            .event("session")
            .data(json!({ "session_id": session_id }).to_string()),
    )]);

    let replay = tokio_stream::iter(
        events
            .into_iter()
            .map(|ev| Ok::<Event, Infallible>(sse_event(ev))),
    );

    let live = TokioStreamExt::filter_map(BroadcastStream::new(rx), |item| match item {
        Ok(ev) => Some(Ok::<Event, Infallible>(sse_event(ev))),
        Err(_) => None,
    });

    let stream = TokioStreamExt::chain(TokioStreamExt::chain(sid_event, replay), live);

    let sse = Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default());

    Ok(sse.into_response())
}

fn sse_event(ev: TranscriptEvent) -> Event {
    let id = ev.ts_ms().to_string();
    let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
    Event::default().event("transcript").id(id).data(data)
}

// ---------------- optional: WS endpoint ----------------

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum WsIn {
    #[serde(rename = "send")]
    Send { text: String },
    #[serde(rename = "ping")]
    Ping,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
enum WsOut {
    #[serde(rename = "pong")]
    Pong,
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "response")]
    Response { session_id: String, text: String },
    #[serde(rename = "transcript")]
    Transcript { event: TranscriptEvent },
}

async fn chat_ws(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    authorize_gateway(&state.http.cfg, &headers)?;
    Ok(ws.on_upgrade(move |socket| chat_ws_task(state, session_key, socket)))
}

async fn chat_ws_task(state: AppState, session_key: String, mut socket: WebSocket) {
    let sub = state.http.engine.subscribe_transcript(&session_key).await;
    let (session_id, rx) = match sub {
        Ok(v) => v,
        Err(e) => {
            let _ = socket
                .send(WsMessage::text(
                    serde_json::to_string(&WsOut::Error {
                        message: e.to_string(),
                    })
                    .unwrap(),
                ))
                .await;
            return;
        }
    };

    let mut rx_stream = BroadcastStream::new(rx);

    loop {
        tokio::select! {
            msg = socket.recv() => {
                let Some(Ok(msg)) = msg else { break };
                match msg {
                    WsMessage::Text(txt) => {
                        match serde_json::from_str::<WsIn>(&txt) {
                            Ok(WsIn::Ping) => {
                                let _ = socket.send(WsMessage::text(serde_json::to_string(&WsOut::Pong).unwrap())).await;
                            }
                            Ok(WsIn::Send{ text }) => {
                                if text.trim().is_empty() { continue; }
                                let out = state.http.engine.handle_message(session_key.clone(), text).await;
                                match out {
                                    Ok(reply) => {
                                        let _ = socket.send(WsMessage::text(
                                            serde_json::to_string(&WsOut::Response{
                                                session_id: reply.session_id,
                                                text: reply.response
                                            }).unwrap()
                                        )).await;
                                    }
                                    Err(e) => {
                                        let _ = socket.send(WsMessage::text(
                                            serde_json::to_string(&WsOut::Error{ message: e.to_string() }).unwrap()
                                        )).await;
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = socket.send(WsMessage::text(
                                    serde_json::to_string(&WsOut::Error{ message: format!("bad json: {e}") }).unwrap()
                                )).await;
                            }
                        }
                    }
                    WsMessage::Close(_) => break,
                    _ => {}
                }
            }

            ev = TokioStreamExt::next(&mut rx_stream) => {
                match ev {
                    Some(Ok(ev)) => {
                        let _ = socket.send(WsMessage::text(
                            serde_json::to_string(&WsOut::Transcript{ event: ev }).unwrap()
                        )).await;
                    }
                    Some(Err(_)) => {}
                    None => break,
                }
            }
        }
    }

    let _ = session_id; // keep if you want to use it for anything else later
}

fn default_name() -> String {
    "default".to_string()
}

async fn get_ui_html(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
) -> Result<impl IntoResponse> {
    authorize_gateway(&state.http.cfg, &headers)?;

    let (exists, html) = state.http.engine.get_ui_html(&session_key).await?;

    Ok((StatusCode::OK, Json(UiHtmlGetResp { exists, html })))
}

async fn ui_html_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_key): Path<String>,
) -> Result<Response> {
    authorize_gateway(&state.http.cfg, &headers)?;

    let last_id: i64 = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);

    // subscribe first
    let rx = state.http.engine.subscribe_ui_html(&session_key).await?;
    let mut stream = BroadcastStream::new(rx);

    let live = TokioStreamExt::filter_map(stream, move |item| {
        let ev = match item {
            Ok(ev) => ev,
            Err(_) => return None,
        };

        // allow reconnect clients to ignore old events by ts_ms if you want:
        let ts = match &ev {
            UiHtmlUpdate::Saved { ts_ms, .. } => *ts_ms,
            UiHtmlUpdate::Deleted { ts_ms, .. } => *ts_ms,
        };
        if ts <= last_id {
            return None;
        }

        let id = ts.to_string();
        let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
        Some(Ok::<Event, Infallible>(
            Event::default().event("ui_html").id(id).data(data),
        ))
    });

    let sse = Sse::new(live).keep_alive(axum::response::sse::KeepAlive::default());
    Ok(sse.into_response())
}

// ---------------- Hooks ----------------

#[derive(Debug, Deserialize)]
struct HookWakeRequest {
    text: String,
    mode: Option<String>,
}

async fn hooks_wake(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<HookWakeRequest>,
) -> Result<impl IntoResponse> {
    authorize_hooks(&state.http.cfg, &headers)?;

    if req.text.trim().is_empty() {
        return Ok((StatusCode::BAD_REQUEST, Json(json_err("text is required"))));
    }

    // state.http.engine.hooks_wake(req.text, req.mode).await?;
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

#[derive(Debug, Deserialize)]
struct HookAgentRequest {
    message: String,
    name: Option<String>,
    sessionKey: Option<String>,
    wakeMode: Option<String>,
    deliver: Option<bool>,
    channel: Option<String>,
    to: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
    timeoutSeconds: Option<u64>,
}

async fn hooks_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<HookAgentRequest>,
) -> Result<impl IntoResponse> {
    authorize_hooks(&state.http.cfg, &headers)?;

    if req.message.trim().is_empty() {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json_err("message is required")),
        ));
    }

    // let out = state.http.engine.hooks_agent(req).await?;
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}

async fn hooks_named(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(_name): Path<String>,
    Json(_payload): Json<Value>,
) -> Result<impl IntoResponse> {
    authorize_hooks(&state.http.cfg, &headers)?;
    // let out = state.http.engine.hooks_named(name, payload).await?;
    Ok((StatusCode::OK, Json(json!({ "ok": true }))))
}
