use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use futures_util::future::{AbortHandle, Abortable};
use leptos::logging::log;
use leptos::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::components::chat::api::TranscriptSse;
use crate::components::transcript::TranscriptList;
use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};
use crate::components::ui::dialog::*;

use crabbot_shared::api::transcript::TranscriptEvent;

use super::api::{api_get_transcript, api_list_sessions, api_post_message, api_stream_transcript};
use super::session_cell::SessionCell;
use super::top_bar::ChatTopBar;
use super::types::SessionEventsMap;
use super::utils::latest_html;

fn last_ts_ms(events: &[TranscriptEvent]) -> i64 {
    events.last().map(|e| e.ts_ms()).unwrap_or(0)
}

#[component]
pub fn ChatUiConnected() -> impl IntoView {
    let base_http = "".to_string();

    let (session_keys, set_session_keys) = create_signal::<Vec<String>>(vec![]);
    let (main_session, set_main_session) = create_signal::<Option<String>>(None);

    let sessions_events = create_rw_signal::<SessionEventsMap>(HashMap::new());

    let (draft, set_draft) = create_signal(String::new());
    let (err, set_err) = create_signal::<Option<String>>(None);
    let (history_open, set_history_open) = create_signal(false);

    let (modal_session, set_modal_session) = create_signal::<Option<String>>(None);

    let sse_handle: Rc<RefCell<Option<TranscriptSse>>> = Rc::new(RefCell::new(None));

    // initial load sessions: pick user:me
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
                            .find(|k| k.as_str() == "user:me")
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

    let on_new_chat = {
        // if you really want a new session, you need a backend create endpoint.
        // for now, just jump to "user:me".
        let set_main_session = set_main_session.clone();
        let set_history_open = set_history_open.clone();
        Callback::new(move |_| {
            set_main_session.set(Some("user:me".into()));
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

            log!("clicked send sessions_events");
            // sessions_events.update(|m| {
            //     m.entry(sk.clone()).or_default().push(
            //         crabbot_shared::api::transcript::TranscriptEvent::user(text.clone()),
            //     );
            // });
            // log!("clicked send sessions_events 2");
            set_draft.set(String::new());

            let base_http2 = base_http.clone();
            let sk2 = sk.clone();
            let set_err2 = set_err.clone();

            spawn_local(async move {
                log!("clicked send api_post_message ");
                if let Err(e) = api_post_message(&base_http2, &sk2, text).await {
                    set_err2.set(Some(e));
                }
            });
        })
    };

    let on_open_history = {
        let set_modal_session = set_modal_session.clone();
        Callback::new(move |sk: String| {
            set_modal_session.set(Some(sk));
            // your Dialog opens via trigger click; keep your existing mechanism
            // (not re-adding it here since you already have it wired elsewhere)
        })
    };

    let main_html = Signal::derive({
        let sessions_events = sessions_events.clone();
        let main_session = main_session.clone();
        move || {
            let Some(sk) = main_session.get() else {
                return None;
            };
            sessions_events
                .with(|map| latest_html(map.get(&sk).map(|v| v.as_slice()).unwrap_or(&[])))
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
                                    let sessions_events = sessions_events.clone();
                                    let k2 = k.clone();

                                    let html = Signal::derive({
                                        let sessions_events = sessions_events.clone();
                                        let k2 = k2.clone();
                                        move || {
                                            sessions_events.with(|map| {
                                                latest_html(map.get(&k2).map(|v| v.as_slice()).unwrap_or(&[]))
                                                    .unwrap_or_else(|| "<div class='text-sm text-muted-foreground'>No content yet</div>".into())
                                            })
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
                                //<div class="truncate text-sm font-semibold">
                                //    {move || main_session.get().unwrap_or_else(|| "No session selected".into())}
                                //</div>
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

            //<Dialog>
            //    <DialogTrigger>
            //        <Button variant=ButtonVariant::Ghost size=ButtonSize::Icon>
            //            "Open History"
            //        </Button>
            //    </DialogTrigger>

            //    <DialogContent class="max-w-3xl">
            //        <DialogBody>
            //            <DialogHeader>
            //                <DialogTitle>
            //                    {move || {
            //                        let sk = modal_session.get().unwrap_or_else(|| "-".into());
            //                        format!("History: {sk}")
            //                    }}
            //                </DialogTitle>
            //            </DialogHeader>

            //            <div class="max-h-[70vh] overflow-auto pr-1">
            //                <Show
            //                    when=move || modal_session.get().is_some()
            //                    fallback=|| view! { <div class="text-sm text-muted-foreground">"No session"</div> }
            //                >
            //                    <TranscriptList events=modal_events />
            //                </Show>
            //            </div>

            //            <DialogFooter>
            //                <DialogClose class="w-full sm:w-fit">
            //                    "Close"
            //                </DialogClose>
            //            </DialogFooter>
            //        </DialogBody>
            //    </DialogContent>
            //</Dialog>
        </div>
    }
}
