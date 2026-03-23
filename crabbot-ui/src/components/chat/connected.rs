use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crabbot_shared::DEFAULT_SESSION_KEY;
use crabbot_shared::api::model::{MemoryResp, SessionInfo, ToolSessionHistoryResp};
use crabbot_shared::api::ui_html::UiHtmlUpdate;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::components::chat::api::{TranscriptSse, UiHtmlSse};
use crate::components::transcript::TranscriptList;

use crabbot_shared::api::transcript::TranscriptEvent;

use super::api::{
    api_get_in_flight_sessions, api_get_memory, api_get_tool_session_history,
    api_get_tool_sessions, api_get_transcript, api_list_sessions_detailed, api_post_message,
    api_stream_transcript, api_update_memory,
};
use super::session_cell::SessionCell;
use super::top_bar::ChatTopBar;
use super::types::SessionEventsMap;

fn last_ts_ms(events: &[TranscriptEvent]) -> i64 {
    events.last().map(|e| e.ts_ms()).unwrap_or(0)
}

#[component]
pub fn ChatUiConnected() -> impl IntoView {
    let base_http = "".to_string();

    let (session_keys, set_session_keys) = create_signal::<Vec<String>>(vec![]);
    let (main_session, set_main_session) = create_signal::<Option<String>>(None);

    // Detailed session info (type, label) keyed by session key
    let (session_infos, set_session_infos) =
        create_signal::<HashMap<String, SessionInfo>>(HashMap::new());

    let sessions_html = create_rw_signal::<HashMap<String, String>>(HashMap::new());
    let ui_sse_handle: Rc<RefCell<Option<UiHtmlSse>>> = Rc::new(RefCell::new(None));

    let sessions_events = create_rw_signal::<SessionEventsMap>(HashMap::new());

    let (draft, set_draft) = create_signal(String::new());
    let (err, set_err) = create_signal::<Option<String>>(None);
    let (history_open, set_history_open) = create_signal(false);

    // Sessions drawer state (replaces left sidebar)
    let (sessions_drawer_open, set_sessions_drawer_open) = create_signal(false);
    // Drawer tab: "sessions" or "memory"
    let (drawer_tab, set_drawer_tab) = create_signal::<String>("sessions".to_string());

    // Memory state
    let (memory_data, set_memory_data) = create_signal::<Option<MemoryResp>>(None);
    // Memory editing state
    let (editing_short_term, set_editing_short_term) = create_signal(false);
    let (editing_daily, set_editing_daily) = create_signal(false);
    let (short_term_draft, set_short_term_draft) = create_signal(String::new());
    let (daily_draft, set_daily_draft) = create_signal(String::new());
    let (memory_saving, set_memory_saving) = create_signal(false);

    // In-flight sessions (running in scheduler)
    let (in_flight_keys, set_in_flight_keys) = create_signal::<Vec<String>>(vec![]);

    // Tool session stats
    let (tool_sessions_stats, set_tool_sessions_stats) =
        create_signal::<Vec<crabbot_shared::api::model::ToolSessionStatsResp>>(vec![]);

    // Tool session history modal
    let (tool_history_modal, set_tool_history_modal) = create_signal::<Option<String>>(None);
    let (tool_history_data, set_tool_history_data) =
        create_signal::<Option<ToolSessionHistoryResp>>(None);
    let (tool_history_loading, set_tool_history_loading) = create_signal(false);

    // modal session (history for "other sessions")
    let (modal_session, set_modal_session) = create_signal::<Option<String>>(None);

    let sse_handle: Rc<RefCell<Option<TranscriptSse>>> = Rc::new(RefCell::new(None));
    // SSE handle for modal transcript streaming
    let modal_sse_handle: Rc<RefCell<Option<TranscriptSse>>> = Rc::new(RefCell::new(None));

    // initial load sessions (detailed): pick default
    {
        let base_http_cp = base_http.clone();
        create_effect(move |_| {
            let base_http_cp2 = base_http_cp.clone();
            spawn_local(async move {
                match api_list_sessions_detailed(&base_http_cp2).await {
                    Ok(infos) => {
                        let keys: Vec<String> = infos.iter().map(|s| s.key.clone()).collect();
                        let info_map: HashMap<String, SessionInfo> =
                            infos.into_iter().map(|s| (s.key.clone(), s)).collect();
                        set_session_infos.set(info_map);
                        set_session_keys.set(keys.clone());
                        let main = keys
                            .iter()
                            .find(|k| k.as_str() == DEFAULT_SESSION_KEY)
                            .cloned()
                            .or_else(|| keys.first().cloned());
                        set_main_session.set(main);
                    }
                    Err(e) => set_err.set(Some(e)),
                }
            });
        });
    }

    // load transcripts for all sessions (best-effort)
    {
        let base_http = base_http.clone();
        create_effect(move |_| {
            let keys = session_keys.get();
            if keys.is_empty() {
                return;
            }

            for sk in keys {
                let base_http = base_http.clone();
                let sk_cp = sk.clone();
                let sessions_events = sessions_events.clone();
                let set_err = set_err.clone();

                spawn_local(async move {
                    match api_get_transcript(&base_http, &sk_cp, Some(0), None).await {
                        Ok(tr) => sessions_events.update(|m| {
                            m.insert(sk_cp.clone(), tr.events);
                        }),
                        Err(e) => set_err.set(Some(format!("transcript {sk_cp}: {e}"))),
                    }
                });
            }
        });
    }

    // main session: refresh transcript + start SSE
    {
        let base_http = base_http.clone();
        let sse_handle = sse_handle.clone();

        create_effect(move |_| {
            let Some(sk) = main_session.get() else {
                return;
            };

            set_err.set(None);

            // close previous SSE
            if let Some(h) = sse_handle.borrow().as_ref() {
                h.close();
            }
            *sse_handle.borrow_mut() = None;

            let base_http2 = base_http.clone();
            let sk2 = sk.clone();
            let sessions_events2 = sessions_events.clone();
            let set_err2 = set_err.clone();
            let sse_handle2 = sse_handle.clone();

            spawn_local(async move {
                // 1) initial transcript
                let tr = match api_get_transcript(&base_http2, &sk2, Some(0), None).await {
                    Ok(tr) => tr,
                    Err(e) => {
                        set_err2.set(Some(e));
                        return;
                    }
                };

                let after = last_ts_ms(&tr.events);

                sessions_events2.update(|m| {
                    m.insert(sk2.clone(), tr.events);
                });

                // 2) start SSE after `after`
                let sessions_events3 = sessions_events2.clone();
                let set_err3 = set_err2.clone();
                let sk3 = sk2.clone();

                match api_stream_transcript(
                    &base_http2,
                    &sk2,
                    after,
                    move |ev| {
                        sessions_events3.update(|m| {
                            m.entry(sk3.clone()).or_default().push(ev);
                        });
                    },
                    move |e| set_err3.set(Some(e)),
                ) {
                    Ok(handle) => {
                        *sse_handle2.borrow_mut() = Some(handle);
                    }
                    Err(e) => set_err2.set(Some(e)),
                }
            });
        });
    }

    // load ui_html for all sessions (best-effort)
    {
        let base_http = base_http.clone();
        create_effect(move |_| {
            let keys = session_keys.get();
            if keys.is_empty() {
                return;
            }

            for sk in keys {
                let base_http = base_http.clone();
                let sk_cp = sk.clone();
                let sessions_html = sessions_html.clone();
                let set_err = set_err.clone();

                spawn_local(async move {
                    match super::api::api_get_ui_html(&base_http, &sk_cp).await {
                        Ok(resp) => {
                            let html = if resp.exists {
                                resp.html
                            } else {
                                String::new()
                            };
                            sessions_html.update(|m| {
                                m.insert(sk_cp.clone(), html);
                            });
                        }
                        Err(e) => set_err.set(Some(format!("ui_html {sk_cp}: {e}"))),
                    }
                });
            }
        });
    }

    // main session: ui_html SSE + initial fetch
    {
        let base_http = base_http.clone();
        let ui_sse_handle = ui_sse_handle.clone();

        create_effect(move |_| {
            let Some(sk) = main_session.get() else {
                return;
            };

            // close previous ui SSE
            if let Some(h) = ui_sse_handle.borrow().as_ref() {
                h.close();
            }
            *ui_sse_handle.borrow_mut() = None;

            let base_http2 = base_http.clone();
            let sk2 = sk.clone();
            let sessions_html2 = sessions_html.clone();
            let set_err2 = set_err.clone();
            let ui_sse_handle2 = ui_sse_handle.clone();

            spawn_local(async move {
                // initial fetch for main session
                match super::api::api_get_ui_html(&base_http2, &sk2).await {
                    Ok(resp) => {
                        sessions_html2.update(|m| {
                            m.insert(
                                sk2.clone(),
                                if resp.exists {
                                    resp.html
                                } else {
                                    String::new()
                                },
                            );
                        });
                    }
                    Err(e) => {
                        set_err2.set(Some(e));
                        // still try SSE, so it can recover later
                    }
                }

                let sessions_html3 = sessions_html2.clone();
                let base_http3 = base_http2.clone();
                let sk3 = sk2.clone();
                let set_err3 = set_err2.clone();

                match super::api::api_stream_ui_html(
                    &base_http2,
                    &sk2,
                    move |_upd: UiHtmlUpdate| {
                        // On any update, re-fetch latest HTML
                        let sessions_html = sessions_html3.clone();
                        let base_http = base_http3.clone();
                        let sk = sk3.clone();
                        let set_err = set_err3.clone();

                        spawn_local(async move {
                            match super::api::api_get_ui_html(&base_http, &sk).await {
                                Ok(resp) => {
                                    sessions_html.update(|m| {
                                        m.insert(
                                            sk.clone(),
                                            if resp.exists {
                                                resp.html
                                            } else {
                                                String::new()
                                            },
                                        );
                                    });
                                }
                                Err(e) => set_err.set(Some(e)),
                            }
                        });
                    },
                    move |e| set_err3.set(Some(e)),
                ) {
                    Ok(handle) => {
                        *ui_sse_handle2.borrow_mut() = Some(handle);
                    }
                    Err(e) => set_err2.set(Some(e)),
                }
            });
        });
    }

    let on_new_chat = {
        let set_main_session = set_main_session.clone();
        let set_history_open = set_history_open.clone();
        Callback::new(move |_| {
            set_main_session.set(Some(DEFAULT_SESSION_KEY.into()));
            set_history_open.set(false);
        })
    };

    let on_send = {
        let set_draft = set_draft.clone();
        let draft = draft.clone();
        let main_session = main_session.clone();
        let base_http = base_http.clone();
        let set_err = set_err.clone();

        Callback::new(move |_| {
            let Some(sk) = main_session.get() else {
                return;
            };

            let text = draft.get().trim().to_string();
            if text.is_empty() {
                return;
            }

            set_draft.set(String::new());

            let base_http2 = base_http.clone();
            let sk2 = sk.clone();
            let set_err2 = set_err.clone();

            spawn_local(async move {
                if let Err(e) = api_post_message(&base_http2, &sk2, text).await {
                    set_err2.set(Some(e));
                }
            });
        })
    };

    // modal: open a session history (SSE will be managed by an effect below)
    let on_open_history = {
        let set_modal_session = set_modal_session.clone();
        Callback::new(move |sk: String| {
            set_modal_session.set(Some(sk));
        })
    };

    // modal session -> fetch transcript + start SSE (auto updates)
    {
        let base_http = base_http.clone();
        let modal_sse_handle = modal_sse_handle.clone();

        create_effect(move |_| {
            let sk = modal_session.get();

            // always close previous modal SSE when selection changes or closes
            if let Some(h) = modal_sse_handle.borrow().as_ref() {
                h.close();
            }
            *modal_sse_handle.borrow_mut() = None;

            let Some(sk) = sk else {
                return; // modal closed
            };

            let base_http2 = base_http.clone();
            let sk2 = sk.clone();
            let sessions_events2 = sessions_events.clone();
            let set_err2 = set_err.clone();
            let modal_sse_handle2 = modal_sse_handle.clone();

            spawn_local(async move {
                // 1) initial transcript for modal
                let tr = match api_get_transcript(&base_http2, &sk2, Some(0), None).await {
                    Ok(tr) => tr,
                    Err(e) => {
                        set_err2.set(Some(format!("transcript {sk2}: {e}")));
                        return;
                    }
                };

                let after = last_ts_ms(&tr.events);

                sessions_events2.update(|m| {
                    m.insert(sk2.clone(), tr.events);
                });

                // 2) start SSE after `after`
                let sessions_events3 = sessions_events2.clone();
                let set_err3 = set_err2.clone();
                let sk3 = sk2.clone();

                match api_stream_transcript(
                    &base_http2,
                    &sk2,
                    after,
                    move |ev| {
                        sessions_events3.update(|m| {
                            m.entry(sk3.clone()).or_default().push(ev);
                        });
                    },
                    move |e| set_err3.set(Some(e)),
                ) {
                    Ok(handle) => {
                        *modal_sse_handle2.borrow_mut() = Some(handle);
                    }
                    Err(e) => set_err2.set(Some(e)),
                }
            });
        });
    }

    // Derived signals

    let main_html = Signal::derive({
        let sessions_html = sessions_html.clone();
        let main_session = main_session.clone();
        move || {
            let Some(sk) = main_session.get() else {
                return None;
            };
            sessions_html.with(|m| m.get(&sk).cloned())
        }
    });

    let main_events = Signal::derive({
        let sessions_events = sessions_events.clone();
        let main_session = main_session.clone();
        move || {
            let Some(sk) = main_session.get() else {
                return vec![];
            };
            sessions_events.with(|map| map.get(&sk).cloned().unwrap_or_default())
        }
    });

    let modal_events = Signal::derive({
        let sessions_events = sessions_events.clone();
        let modal_session = modal_session.clone();
        move || {
            let Some(sk) = modal_session.get() else {
                return vec![];
            };
            sessions_events.with(|map| map.get(&sk).cloned().unwrap_or_default())
        }
    });

    let other_sessions = Signal::derive({
        let session_keys = session_keys.clone();
        let main_session = main_session.clone();
        move || {
            let keys = session_keys.get();
            let main = main_session.get();
            keys.into_iter()
                .filter(|k| Some(k.clone()) != main)
                .collect::<Vec<_>>()
        }
    });

    let other_session_count = Signal::derive({
        let other_sessions = other_sessions.clone();
        move || other_sessions.get().len()
    });

    // Load memory when drawer tab switches to "memory"
    {
        let base_http = base_http.clone();
        create_effect(move |_| {
            let tab = drawer_tab.get();
            if tab == "memory" {
                // Reset edit modes when switching to memory tab
                set_editing_short_term.set(false);
                set_editing_daily.set(false);
                let base_http2 = base_http.clone();
                spawn_local(async move {
                    match api_get_memory(&base_http2).await {
                        Ok(resp) => set_memory_data.set(Some(resp)),
                        Err(e) => set_err.set(Some(format!("memory: {e}"))),
                    }
                });
            }
        });
    }

    // Load tool sessions when drawer opens on sessions tab
    {
        let base_http = base_http.clone();
        create_effect(move |_| {
            let tab = drawer_tab.get();
            let drawer_open = sessions_drawer_open.get();
            if drawer_open && tab == "sessions" {
                let base_http2 = base_http.clone();
                spawn_local(async move {
                    match api_get_tool_sessions(&base_http2).await {
                        Ok(stats) => set_tool_sessions_stats.set(stats),
                        Err(_) => {} // silently ignore
                    }
                });
            }
        });
    }

    // Poll in-flight sessions every 2 seconds
    {
        let base_http = base_http.clone();
        spawn_local(async move {
            loop {
                match api_get_in_flight_sessions(&base_http).await {
                    Ok(keys) => set_in_flight_keys.set(keys),
                    Err(_) => {} // silently ignore polling errors
                }
                gloo_timers::future::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
    }

    // Fetch tool session history when modal opens
    {
        let base_http = base_http.clone();
        create_effect(move |_| {
            let tool_name = tool_history_modal.get();
            let Some(name) = tool_name else {
                set_tool_history_data.set(None);
                return;
            };

            set_tool_history_loading.set(true);
            set_tool_history_data.set(None);

            let base_http2 = base_http.clone();
            spawn_local(async move {
                match api_get_tool_session_history(&base_http2, &name).await {
                    Ok(history) => {
                        set_tool_history_data.set(Some(history));
                    }
                    Err(e) => {
                        set_err.set(Some(format!("tool history: {e}")));
                    }
                }
                set_tool_history_loading.set(false);
            });
        });
    }

    // Clone base_http for use inside the memory tab view closure
    let base_http_for_memory = base_http.clone();

    view! {
        <div class="relative h-screen w-full bg-background overflow-hidden">
            // Main content area: full width, padded below top bar
            <div class="h-full w-full pt-16">
                <div class="h-full w-full flex flex-col">
                    // Status bar
                    <div class="flex h-8 items-center gap-2 border-b px-4 shrink-0 overflow-x-auto">
                        <Show when=move || err.get().is_some() fallback=move || {
                            view! {
                                <div class="flex items-center gap-2 text-xs">
                                    <span class="inline-flex items-center gap-1.5">
                                        <span class="relative flex h-2 w-2">
                                            <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-emerald-400 opacity-75"></span>
                                            <span class="relative inline-flex h-2 w-2 rounded-full bg-emerald-500"></span>
                                        </span>
                                        <span class="text-muted-foreground">"Connected"</span>
                                    </span>
                                    <Show when=move || !in_flight_keys.get().is_empty() fallback=|| ()>
                                        <span class="text-muted-foreground/50">"·"</span>
                                        <For
                                            each=move || in_flight_keys.get()
                                            key=|k| k.clone()
                                            children=move |k: String| {
                                                view! {
                                                    <span class="inline-flex items-center gap-1 rounded-full border border-blue-500/20 bg-blue-500/10 px-2 py-0.5 text-[10px] font-medium text-blue-600 dark:text-blue-400">
                                                        <span class="relative flex h-1.5 w-1.5">
                                                            <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-blue-400 opacity-75"></span>
                                                            <span class="relative inline-flex h-1.5 w-1.5 rounded-full bg-blue-500"></span>
                                                        </span>
                                                        {k}
                                                    </span>
                                                }
                                            }
                                        />
                                    </Show>
                                </div>
                            }
                        }>
                            <div class="flex items-center gap-1.5 text-xs">
                                <span class="h-2 w-2 rounded-full bg-red-500"></span>
                                <span class="text-red-600 dark:text-red-400 truncate">{move || err.get().unwrap_or_default()}</span>
                            </div>
                        </Show>
                    </div>

                    // Main HTML content (full width, scrollable)
                    <div class="flex-1 overflow-auto">
                        <div class="h-full w-full">
                            <div
                                class="h-full w-full"
                                inner_html=move || {
                                    main_html.get().unwrap_or_else(|| {
                                        "<div class='py-12 text-center text-muted-foreground'>No content yet</div>".into()
                                    })
                                }
                            />
                        </div>
                    </div>
                </div>
            </div>

            // Top bar (fixed)
            <div class="fixed left-0 right-0 top-0 z-50 pointer-events-none">
                <div class="pointer-events-auto">
                    <ChatTopBar
                        main_session=main_session.into()
                        draft=draft.into()
                        set_draft=set_draft
                        history_open=history_open.into()
                        set_history_open=set_history_open
                        set_sessions_drawer_open=set_sessions_drawer_open
                        on_send=on_send
                        on_new_chat=on_new_chat
                        history_events=main_events
                        other_session_count=other_session_count
                    />
                </div>
            </div>

            // Sessions drawer overlay (slide-in from left)
            <Show when=move || sessions_drawer_open.get() fallback=|| ()>
                // Backdrop
                <div
                    class="fixed inset-0 z-[55] bg-black/40 backdrop-blur-sm transition-opacity"
                    on:click=move |_| set_sessions_drawer_open.set(false)
                />

                // Drawer panel
                <div class="fixed inset-y-0 left-0 z-[56] w-[min(420px,85vw)] bg-background border-r shadow-2xl overflow-hidden flex flex-col animate-slide-in-left">
                    // Drawer header with tabs
                    <div class="border-b shrink-0">
                        <div class="flex items-center justify-between px-4 py-2">
                            <div class="flex gap-1">
                                <button
                                    type="button"
                                    class=move || {
                                        let base = "px-3 py-1.5 rounded-lg text-xs font-medium transition-colors";
                                        if drawer_tab.get() == "sessions" {
                                            format!("{base} bg-primary text-primary-foreground")
                                        } else {
                                            format!("{base} text-muted-foreground hover:bg-accent")
                                        }
                                    }
                                    on:click=move |_| set_drawer_tab.set("sessions".to_string())
                                >
                                    "Sessions"
                                </button>
                                <button
                                    type="button"
                                    class=move || {
                                        let base = "px-3 py-1.5 rounded-lg text-xs font-medium transition-colors";
                                        if drawer_tab.get() == "memory" {
                                            format!("{base} bg-primary text-primary-foreground")
                                        } else {
                                            format!("{base} text-muted-foreground hover:bg-accent")
                                        }
                                    }
                                    on:click=move |_| set_drawer_tab.set("memory".to_string())
                                >
                                    "Memory"
                                </button>
                            </div>
                            <button
                                type="button"
                                class="h-8 w-8 rounded-md hover:bg-accent flex items-center justify-center"
                                on:click=move |_| set_sessions_drawer_open.set(false)
                            >
                                "×"
                            </button>
                        </div>
                    </div>

                    // Tab content
                    <Show when=move || drawer_tab.get() == "sessions" fallback={
                        let base_http_for_memory = base_http_for_memory.clone();
                        move || {
                        // Memory tab content (editable)
                        let base_http_mem = base_http_for_memory.clone();
                        view! {
                            <div class="flex-1 overflow-auto p-4 space-y-4">
                                <Show when=move || memory_data.get().is_some() fallback=move || {
                                    view! {
                                        <div class="py-8 text-center text-sm text-muted-foreground">
                                            "Loading memory..."
                                        </div>
                                    }
                                }>
                                    {
                                        let base_http_mem = base_http_mem.clone();
                                        move || {
                                        let mem = memory_data.get().unwrap();
                                        let base_http_st = base_http_mem.clone();
                                        let base_http_dy = base_http_mem.clone();
                                        let daily_date_for_save = mem.daily_date.clone();
                                        view! {
                                            // Short-term memory section
                                            <div>
                                                <div class="flex items-center justify-between mb-2">
                                                    <div class="text-xs font-semibold text-foreground">
                                                        "Short-term Memory"
                                                    </div>
                                                    <Show when=move || !editing_short_term.get() fallback=move || {
                                                        let base_http_save = base_http_st.clone();
                                                        view! {
                                                            <div class="flex gap-1">
                                                                <button
                                                                    type="button"
                                                                    class="px-2 py-0.5 rounded text-[10px] font-medium bg-primary text-primary-foreground disabled:opacity-50"
                                                                    disabled=move || memory_saving.get()
                                                                    on:click=move |_| {
                                                                        let content = short_term_draft.get();
                                                                        let base = base_http_save.clone();
                                                                        set_memory_saving.set(true);
                                                                        spawn_local(async move {
                                                                            match api_update_memory(&base, "short_term", content.clone(), None).await {
                                                                                Ok(()) => {
                                                                                    set_memory_data.update(|d| {
                                                                                        if let Some(m) = d {
                                                                                            m.short_term = content;
                                                                                        }
                                                                                    });
                                                                                    set_editing_short_term.set(false);
                                                                                }
                                                                                Err(e) => set_err.set(Some(format!("save memory: {e}"))),
                                                                            }
                                                                            set_memory_saving.set(false);
                                                                        });
                                                                    }
                                                                >
                                                                    {move || if memory_saving.get() { "Saving..." } else { "Save" }}
                                                                </button>
                                                                <button
                                                                    type="button"
                                                                    class="px-2 py-0.5 rounded text-[10px] font-medium text-muted-foreground hover:bg-accent"
                                                                    on:click=move |_| set_editing_short_term.set(false)
                                                                >
                                                                    "Cancel"
                                                                </button>
                                                            </div>
                                                        }
                                                    }>
                                                        <button
                                                            type="button"
                                                            class="px-2 py-0.5 rounded text-[10px] font-medium text-muted-foreground hover:bg-accent"
                                                            on:click=move |_| {
                                                                let current = memory_data.get().map(|m| m.short_term.clone()).unwrap_or_default();
                                                                set_short_term_draft.set(current);
                                                                set_editing_short_term.set(true);
                                                            }
                                                        >
                                                            "Edit"
                                                        </button>
                                                    </Show>
                                                </div>
                                                <Show when=move || editing_short_term.get() fallback=move || {
                                                    let st = mem.short_term.clone();
                                                    view! {
                                                        <div class="rounded-lg border bg-card p-3">
                                                            <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">
                                                                {if st.is_empty() {
                                                                    "(empty)".to_string()
                                                                } else {
                                                                    st
                                                                }}
                                                            </pre>
                                                        </div>
                                                    }
                                                }>
                                                    <textarea
                                                        class="w-full rounded-lg border bg-card p-3 text-xs font-mono leading-relaxed text-foreground resize-y min-h-[120px] outline-none focus:ring-2 focus:ring-ring/50"
                                                        prop:value=move || short_term_draft.get()
                                                        on:input=move |e| set_short_term_draft.set(event_target_value(&e))
                                                    />
                                                </Show>
                                            </div>
                                            // Daily memory section
                                            <div>
                                                <div class="flex items-center justify-between mb-2">
                                                    <div class="text-xs font-semibold text-foreground">
                                                        {format!("Daily Memory ({})", mem.daily_date)}
                                                    </div>
                                                    <Show when=move || !editing_daily.get() fallback=move || {
                                                        let base_http_save = base_http_dy.clone();
                                                        let dd = daily_date_for_save.clone();
                                                        view! {
                                                            <div class="flex gap-1">
                                                                <button
                                                                    type="button"
                                                                    class="px-2 py-0.5 rounded text-[10px] font-medium bg-primary text-primary-foreground disabled:opacity-50"
                                                                    disabled=move || memory_saving.get()
                                                                    on:click=move |_| {
                                                                        let content = daily_draft.get();
                                                                        let base = base_http_save.clone();
                                                                        let date = dd.clone();
                                                                        set_memory_saving.set(true);
                                                                        spawn_local(async move {
                                                                            match api_update_memory(&base, "daily", content.clone(), Some(date)).await {
                                                                                Ok(()) => {
                                                                                    set_memory_data.update(|d| {
                                                                                        if let Some(m) = d {
                                                                                            m.daily = content;
                                                                                        }
                                                                                    });
                                                                                    set_editing_daily.set(false);
                                                                                }
                                                                                Err(e) => set_err.set(Some(format!("save memory: {e}"))),
                                                                            }
                                                                            set_memory_saving.set(false);
                                                                        });
                                                                    }
                                                                >
                                                                    {move || if memory_saving.get() { "Saving..." } else { "Save" }}
                                                                </button>
                                                                <button
                                                                    type="button"
                                                                    class="px-2 py-0.5 rounded text-[10px] font-medium text-muted-foreground hover:bg-accent"
                                                                    on:click=move |_| set_editing_daily.set(false)
                                                                >
                                                                    "Cancel"
                                                                </button>
                                                            </div>
                                                        }
                                                    }>
                                                        <button
                                                            type="button"
                                                            class="px-2 py-0.5 rounded text-[10px] font-medium text-muted-foreground hover:bg-accent"
                                                            on:click=move |_| {
                                                                let current = memory_data.get().map(|m| m.daily.clone()).unwrap_or_default();
                                                                set_daily_draft.set(current);
                                                                set_editing_daily.set(true);
                                                            }
                                                        >
                                                            "Edit"
                                                        </button>
                                                    </Show>
                                                </div>
                                                <Show when=move || editing_daily.get() fallback=move || {
                                                    let dy = mem.daily.clone();
                                                    view! {
                                                        <div class="rounded-lg border bg-card p-3">
                                                            <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed">
                                                                {if dy.is_empty() {
                                                                    "(empty)".to_string()
                                                                } else {
                                                                    dy
                                                                }}
                                                            </pre>
                                                        </div>
                                                    }
                                                }>
                                                    <textarea
                                                        class="w-full rounded-lg border bg-card p-3 text-xs font-mono leading-relaxed text-foreground resize-y min-h-[120px] outline-none focus:ring-2 focus:ring-ring/50"
                                                        prop:value=move || daily_draft.get()
                                                        on:input=move |e| set_daily_draft.set(event_target_value(&e))
                                                    />
                                                </Show>
                                            </div>
                                        }
                                    }}
                                </Show>
                            </div>
                        }
                    }}>
                        // Sessions tab content
                        <div class="flex-1 overflow-auto p-3 space-y-2">
                            <Show when=move || other_sessions.get().is_empty() fallback=|| ()>
                                <div class="py-8 text-center text-sm text-muted-foreground">
                                    "No other sessions"
                                </div>
                            </Show>

                            <For
                                each=move || other_sessions.get()
                                key=|k| k.clone()
                                children=move |k: String| {
                                    let k2 = k.clone();
                                    let k3 = k.clone();

                                    let html = Signal::derive({
                                        let sessions_html = sessions_html.clone();
                                        let k2 = k2.clone();
                                        move || {
                                            sessions_html
                                                .with(|m| m.get(&k2).cloned())
                                                .unwrap_or_else(|| "<div class='text-sm text-muted-foreground'>No content yet</div>".into())
                                        }
                                    });

                                    let info = Signal::derive({
                                        let k3 = k3.clone();
                                        move || {
                                            session_infos.get().get(&k3).cloned()
                                        }
                                    });

                                    let stype = Signal::derive({
                                        let info = info.clone();
                                        move || info.get().map(|i| i.session_type.clone()).unwrap_or_default()
                                    });

                                    let slabel = Signal::derive({
                                        let info = info.clone();
                                        move || info.get().map(|i| i.label.clone()).unwrap_or_default()
                                    });

                                    let is_running = Signal::derive({
                                        let k_run = k3.clone();
                                        move || in_flight_keys.get().contains(&k_run)
                                    });

                                    view! {
                                        <SessionCell
                                            session_key=k
                                            session_type=stype.get()
                                            label=slabel.get()
                                            html=html
                                            on_open_history=on_open_history
                                            running=is_running.get()
                                        />
                                    }
                                }
                            />

                            // Tool session stats
                            <Show when=move || !tool_sessions_stats.get().is_empty() fallback=|| ()>
                                <div class="mt-4 pt-3 border-t">
                                    <div class="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider mb-2">"Tool Sessions"</div>
                                    <div class="space-y-1.5">
                                        <For
                                            each=move || tool_sessions_stats.get()
                                            key=|s| s.tool_name.clone()
                                            children=move |stat: crabbot_shared::api::model::ToolSessionStatsResp| {
                                                let tool_name_for_click = stat.tool_name.clone();
                                                view! {
                                                    <div
                                                        class="rounded-lg border bg-card/50 px-3 py-2 cursor-pointer hover:border-foreground/20 transition-colors"
                                                        on:click=move |_| {
                                                            set_tool_history_modal.set(Some(tool_name_for_click.clone()));
                                                        }
                                                    >
                                                        <div class="flex items-center justify-between">
                                                            <div class="flex items-center gap-2">
                                                                <span class="inline-flex items-center rounded-md border border-cyan-500/20 bg-cyan-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-cyan-700 dark:text-cyan-400">
                                                                    "Tool"
                                                                </span>
                                                                <span class="text-xs font-medium text-foreground">{stat.tool_name.clone()}</span>
                                                            </div>
                                                            <div class="flex items-center gap-3 text-[10px] text-muted-foreground">
                                                                <span>{format!("{} calls", stat.total_calls)}</span>
                                                                <Show when=move || { stat.total_errors > 0 } fallback=|| ()>
                                                                    <span class="text-red-500">{format!("{} errors", stat.total_errors)}</span>
                                                                </Show>
                                                                <span class="text-muted-foreground/60">"▸"</span>
                                                            </div>
                                                        </div>
                                                    </div>
                                                }
                                            }
                                        />
                                    </div>
                                </div>
                            </Show>
                        </div>
                    </Show>
                </div>
            </Show>

            // Tool session history modal
            <Show when=move || tool_history_modal.get().is_some() fallback=|| ()>
                <div class="fixed inset-0 z-[60] bg-background/70 backdrop-blur-sm">
                    <div class="absolute left-1/2 top-1/2 w-[min(900px,95vw)] h-[min(80vh,900px)] -translate-x-1/2 -translate-y-1/2 rounded-2xl border bg-background shadow-2xl overflow-hidden flex flex-col">
                        <div class="flex items-center justify-between border-b px-4 py-3 shrink-0">
                            <div class="flex items-center gap-2">
                                <span class="inline-flex items-center rounded-md border border-cyan-500/20 bg-cyan-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-cyan-700 dark:text-cyan-400">
                                    "Tool"
                                </span>
                                <span class="text-xs font-medium text-foreground">
                                    {move || tool_history_modal.get().unwrap_or_else(|| "-".into())}
                                </span>
                                {move || {
                                    tool_history_data.get().map(|d| {
                                        view! {
                                            <span class="text-[10px] text-muted-foreground ml-2">
                                                {format!("{} calls · {} errors · {} compactions",
                                                    d.total_calls, d.total_errors, d.compaction_count)}
                                            </span>
                                        }
                                    })
                                }}
                            </div>
                            <button
                                type="button"
                                class="h-8 w-8 rounded-md hover:bg-accent flex items-center justify-center"
                                on:click=move |_| set_tool_history_modal.set(None)
                            >
                                "×"
                            </button>
                        </div>

                        <div class="flex-1 overflow-auto">
                            <Show when=move || tool_history_loading.get() fallback=|| ()>
                                <div class="py-8 text-center text-sm text-muted-foreground">
                                    "Loading..."
                                </div>
                            </Show>

                            <Show when=move || tool_history_data.get().is_some() fallback=|| ()>
                                {move || {
                                    let data = tool_history_data.get();
                                    let entries = data.map(|d| d.entries).unwrap_or_default();

                                    if entries.is_empty() {
                                        return view! {
                                            <div class="py-8 text-center text-sm text-muted-foreground">
                                                "No entries yet"
                                            </div>
                                        }.into_any();
                                    }

                                    view! {
                                        <div class="divide-y">
                                            {entries.into_iter().map(|entry| {
                                                use crabbot_shared::api::model::ToolSessionEntryKindResp;
                                                let ts = {
                                                    let date = js_sys::Date::new_0();
                                                    date.set_time(entry.ts_ms as f64);
                                                    let h = date.get_hours();
                                                    let m = date.get_minutes();
                                                    let s = date.get_seconds();
                                                    format!("{:02}:{:02}:{:02}", h, m, s)
                                                };

                                                match &entry.kind {
                                                    ToolSessionEntryKindResp::Call { call_id, args_summary } => {
                                                        let call_id = call_id.clone();
                                                        let args_summary = args_summary.clone();
                                                        view! {
                                                            <div class="px-4 py-2.5 hover:bg-accent/50">
                                                                <div class="flex items-center gap-2 mb-1">
                                                                    <span class="inline-flex items-center rounded-md border border-blue-500/20 bg-blue-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-blue-700 dark:text-blue-400">
                                                                        "Call"
                                                                    </span>
                                                                    <span class="text-[10px] text-muted-foreground font-mono">{ts}</span>
                                                                    <span class="text-[10px] text-muted-foreground/60 font-mono">{call_id}</span>
                                                                </div>
                                                                <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-all font-mono leading-relaxed mt-1">{args_summary}</pre>
                                                            </div>
                                                        }.into_any()
                                                    }
                                                    ToolSessionEntryKindResp::Result { call_id, success, output_summary } => {
                                                        let call_id = call_id.clone();
                                                        let output_summary = output_summary.clone();
                                                        let success = *success;
                                                        let (badge_text, badge_class) = if success {
                                                            ("OK", "border-green-500/20 bg-green-500/15 text-green-700 dark:text-green-400")
                                                        } else {
                                                            ("FAIL", "border-red-500/20 bg-red-500/15 text-red-700 dark:text-red-400")
                                                        };
                                                        view! {
                                                            <div class="px-4 py-2.5 hover:bg-accent/50">
                                                                <div class="flex items-center gap-2 mb-1">
                                                                    <span class=format!(
                                                                        "inline-flex items-center rounded-md border px-1.5 py-0.5 text-[10px] font-semibold leading-none {}",
                                                                        badge_class
                                                                    )>
                                                                        {badge_text}
                                                                    </span>
                                                                    <span class="text-[10px] text-muted-foreground font-mono">{ts}</span>
                                                                    <span class="text-[10px] text-muted-foreground/60 font-mono">{call_id}</span>
                                                                </div>
                                                                <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-all font-mono leading-relaxed mt-1">{output_summary}</pre>
                                                            </div>
                                                        }.into_any()
                                                    }
                                                    ToolSessionEntryKindResp::Error { call_id, error } => {
                                                        let call_id = call_id.clone();
                                                        let error = error.clone();
                                                        view! {
                                                            <div class="px-4 py-2.5 hover:bg-accent/50">
                                                                <div class="flex items-center gap-2 mb-1">
                                                                    <span class="inline-flex items-center rounded-md border border-red-500/20 bg-red-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-red-700 dark:text-red-400">
                                                                        "Error"
                                                                    </span>
                                                                    <span class="text-[10px] text-muted-foreground font-mono">{ts}</span>
                                                                    <span class="text-[10px] text-muted-foreground/60 font-mono">{call_id}</span>
                                                                </div>
                                                                <pre class="text-xs text-red-600 dark:text-red-400 whitespace-pre-wrap break-all font-mono leading-relaxed mt-1">{error}</pre>
                                                            </div>
                                                        }.into_any()
                                                    }
                                                    ToolSessionEntryKindResp::CompactionSummary { summary, entries_compacted, .. } => {
                                                        let summary = summary.clone();
                                                        let entries_compacted = *entries_compacted;
                                                        view! {
                                                            <div class="px-4 py-2.5 hover:bg-accent/50">
                                                                <div class="flex items-center gap-2 mb-1">
                                                                    <span class="inline-flex items-center rounded-md border border-violet-500/20 bg-violet-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-violet-700 dark:text-violet-400">
                                                                        "Summary"
                                                                    </span>
                                                                    <span class="text-[10px] text-muted-foreground font-mono">{ts}</span>
                                                                    <span class="text-[10px] text-muted-foreground/60">{format!("{} entries compacted", entries_compacted)}</span>
                                                                </div>
                                                                <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed mt-1">{summary}</pre>
                                                            </div>
                                                        }.into_any()
                                                    }
                                                    ToolSessionEntryKindResp::Note { text } => {
                                                        let text = text.clone();
                                                        view! {
                                                            <div class="px-4 py-2.5 hover:bg-accent/50">
                                                                <div class="flex items-center gap-2 mb-1">
                                                                    <span class="inline-flex items-center rounded-md border border-amber-500/20 bg-amber-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-amber-700 dark:text-amber-400">
                                                                        "Note"
                                                                    </span>
                                                                    <span class="text-[10px] text-muted-foreground font-mono">{ts}</span>
                                                                </div>
                                                                <pre class="text-xs text-muted-foreground whitespace-pre-wrap break-words font-mono leading-relaxed mt-1">{text}</pre>
                                                            </div>
                                                        }.into_any()
                                                    }
                                                }
                                            }).collect::<Vec<_>>()}
                                        </div>
                                    }.into_any()
                                }}
                            </Show>
                        </div>
                    </div>
                </div>
            </Show>

            // Modal overlay for viewing individual session transcript (auto-updates via SSE)
            <Show when=move || modal_session.get().is_some() fallback=|| ()>
                <div class="fixed inset-0 z-[60] bg-background/70 backdrop-blur-sm">
                    <div class="absolute left-1/2 top-1/2 w-[min(900px,95vw)] h-[min(80vh,900px)] -translate-x-1/2 -translate-y-1/2 rounded-2xl border bg-background shadow-2xl overflow-hidden">
                        <div class="flex items-center justify-between border-b px-4 py-3">
                            <div class="text-xs font-medium text-muted-foreground">
                                {move || format!("History: {}", modal_session.get().unwrap_or_else(|| "-".into()))}
                            </div>
                            <button
                                type="button"
                                class="h-8 w-8 rounded-md hover:bg-accent flex items-center justify-center"
                                on:click=move |_| set_modal_session.set(None)
                            >
                                "×"
                            </button>
                        </div>

                        <TranscriptList events=modal_events height_class="h-[calc(100%-48px)]".to_string() />
                    </div>
                </div>
            </Show>
        </div>
    }
}
