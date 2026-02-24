use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crabbot_shared::DEFAULT_SESSION_KEY;
use crabbot_shared::api::ui_html::UiHtmlUpdate;
use futures_util::future::{AbortHandle, Abortable};
use leptos::logging::log;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::components::chat::api::{TranscriptSse, UiHtmlSse};
use crate::components::transcript::TranscriptList;
use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::components::ui::dialog::*;

use crabbot_shared::api::transcript::TranscriptEvent;

use super::api::{api_get_transcript, api_list_sessions, api_post_message, api_stream_transcript};
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

    let sessions_html = create_rw_signal::<HashMap<String, String>>(HashMap::new());
    let ui_sse_handle: Rc<RefCell<Option<UiHtmlSse>>> = Rc::new(RefCell::new(None));

    let sessions_events = create_rw_signal::<SessionEventsMap>(HashMap::new());

    let (draft, set_draft) = create_signal(String::new());
    let (err, set_err) = create_signal::<Option<String>>(None);
    let (history_open, set_history_open) = create_signal(false);

    // modal session (history for "other sessions")
    let (modal_session, set_modal_session) = create_signal::<Option<String>>(None);

    let sse_handle: Rc<RefCell<Option<TranscriptSse>>> = Rc::new(RefCell::new(None));
    // NEW: SSE handle for modal transcript streaming
    let modal_sse_handle: Rc<RefCell<Option<TranscriptSse>>> = Rc::new(RefCell::new(None));

    // initial load sessions: pick default
    {
        let base_http_cp = base_http.clone();
        create_effect(move |_| {
            let base_http_cp2 = base_http_cp.clone();
            spawn_local(async move {
                match api_list_sessions(&base_http_cp2).await {
                    Ok(keys) => {
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

    // NEW: modal session -> fetch transcript + start SSE (auto updates)
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

    view! {
        <div class="relative h-screen w-full bg-background overflow-hidden">
            <div class="h-full w-full pt-16">
                <div class="grid h-full w-full grid-cols-[360px_1fr] gap-3 p-4">
                    <div class="h-full overflow-auto p-2 border-r">
                        <div class="px-2 pb-2 text-xs font-medium text-muted-foreground">
                            "Other sessions"
                        </div>

                        <div class="space-y-2">
                            <For
                                each=move || other_sessions.get()
                                key=|k| k.clone()
                                children=move |k: String| {
                                    let k2 = k.clone();

                                    let html = Signal::derive({
                                        let sessions_html = sessions_html.clone();
                                        let k2 = k2.clone();
                                        move || {
                                            sessions_html
                                                .with(|m| m.get(&k2).cloned())
                                                .unwrap_or_else(|| "<div class='text-sm text-muted-foreground'>No content yet</div>".into())
                                        }
                                    });

                                    view! {
                                        <SessionCell session_key=k html=html on_open_history=on_open_history />
                                    }
                                }
                            />
                        </div>
                    </div>

                    <div class="h-full overflow-hidden bg-background">
                        <div class="flex h-12 items-center justify-between border-b px-4">
                            <div class="min-w-0">
                                <div class="truncate text-xs text-muted-foreground">
                                    {move || err.get().unwrap_or_else(|| "Connected".into())}
                                </div>
                            </div>
                        </div>

                        <div class="h-[calc(100%-48px)] overflow-auto p-4">
                            <div
                                class="prose dark:prose-invert max-w-none"
                                inner_html=move || {
                                    main_html.get().unwrap_or_else(|| "<div class='text-sm text-muted-foreground'>No HTML yet</div>".into())
                                }
                            />
                        </div>
                    </div>
                </div>
            </div>

            <div class="fixed left-0 right-0 top-0 z-50 pointer-events-none">
                <div class="pointer-events-auto">
                    <ChatTopBar
                        main_session=main_session.into()
                        draft=draft.into()
                        set_draft=set_draft
                        history_open=history_open.into()
                        set_history_open=set_history_open
                        on_send=on_send
                        on_new_chat=on_new_chat
                        history_events=main_events
                    />
                </div>
            </div>

            // modal overlay (auto-updates via SSE)
            <Show when=move || modal_session.get().is_some() fallback=|| ()>
                <div class="fixed inset-0 z-[60] bg-background/70 backdrop-blur-sm">
                    <div class="absolute left-1/2 top-1/2 w-[min(900px,95vw)] h-[min(80vh,900px)] -translate-x-1/2 -translate-y-1/2 rounded-2xl border bg-background shadow-2xl overflow-hidden">
                        <div class="flex items-center justify-between border-b px-4 py-3">
                            <div class="text-xs font-medium text-muted-foreground">
                                {move || format!("History: {}", modal_session.get().unwrap_or_else(|| "-".into()))}
                            </div>
                            <button
                                type="button"
                                class="h-8 w-8 rounded-md hover:bg-accent"
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
