use leptos::prelude::*;
use serde_json::Value as Json;
use std::collections::HashMap;

use leptos::html;
use leptos::prelude::*;
use web_sys::HtmlElement;

use icons::{ChevronDown, ChevronRight};

use crabbot_shared::api::transcript::{
    AssistantEvent, CompactionSummaryEvent, CustomMessageEvent, CustomNoteEvent, ToolCallEvent,
    ToolResultEvent, TranscriptEvent, UserEvent,
};

fn format_time(ts_ms: i64) -> String {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(ts_ms as f64));
    format!("{:02}:{:02}", d.get_hours(), d.get_minutes())
}

fn pretty_json(v: &Json) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())
}

fn truncate_one_line(s: &str, max: usize) -> String {
    let s = s.replace('\n', " ");
    if s.chars().count() <= max {
        return s;
    }
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i >= max {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

// ========================= grouping =========================

#[derive(Clone, Debug, PartialEq)]
pub enum TranscriptRow {
    Event(TranscriptEvent),
    ToolExchange {
        call_id: String,
        tool: String,
        call_ts_ms: i64,
        call_args: Json,
        result_ts_ms: Option<i64>,
        ok: Option<bool>,
        result_json: Option<Json>,
        error: Option<String>,
    },
}

fn group_transcript_events(events: Vec<TranscriptEvent>) -> Vec<TranscriptRow> {
    // Preserve overall ordering: replace tool call/result events with a single ToolExchange row.
    // If result arrives before call, keep a pending exchange and finalize when call arrives.
    let mut rows: Vec<TranscriptRow> = Vec::with_capacity(events.len());
    let mut index_by_call_id: HashMap<String, usize> = HashMap::new();
    let mut pending_result_by_call_id: HashMap<String, ToolResultEvent> = HashMap::new();

    for ev in events {
        match ev {
            TranscriptEvent::ToolCall(call) => {
                let call_id = call.call_id.clone();

                let mut row = TranscriptRow::ToolExchange {
                    call_id: call_id.clone(),
                    tool: call.tool.clone(),
                    call_ts_ms: call.ts_ms,
                    call_args: call.args_json.clone(),
                    result_ts_ms: None,
                    ok: None,
                    result_json: None,
                    error: None,
                };

                if let Some(res) = pending_result_by_call_id.remove(&call_id) {
                    row = TranscriptRow::ToolExchange {
                        call_id: call_id.clone(),
                        tool: call.tool.clone(),
                        call_ts_ms: call.ts_ms,
                        call_args: call.args_json.clone(),
                        result_ts_ms: Some(res.ts_ms),
                        ok: Some(res.ok),
                        result_json: Some(res.result_json.clone()),
                        error: res.error.clone(),
                    };
                }

                let idx = rows.len();
                rows.push(row);
                index_by_call_id.insert(call_id, idx);
            }

            TranscriptEvent::ToolResult(res) => {
                let call_id = res.call_id.clone();

                if let Some(&idx) = index_by_call_id.get(&call_id) {
                    // Update existing exchange row.
                    if let Some(existing) = rows.get_mut(idx) {
                        if let TranscriptRow::ToolExchange {
                            result_ts_ms,
                            ok,
                            result_json,
                            error,
                            ..
                        } = existing
                        {
                            *result_ts_ms = Some(res.ts_ms);
                            *ok = Some(res.ok);
                            *result_json = Some(res.result_json.clone());
                            *error = res.error.clone();
                        }
                    }
                } else {
                    // Result before call: stash.
                    pending_result_by_call_id.insert(call_id, res);
                }
            }

            other => rows.push(TranscriptRow::Event(other)),
        }
    }

    rows
}

#[component]
pub fn TranscriptList(
    #[prop(into)] events: Signal<Vec<TranscriptEvent>>,

    /// Optional: fixed height class for the scroll container.
    /// Defaults to `h-[50vh]`.
    #[prop(optional)]
    height_class: Option<String>,
) -> impl IntoView {
    let rows = create_memo(move |_| group_transcript_events(events.get()));

    let scroll_ref = create_node_ref::<html::Div>();
    let (pinned_to_bottom, set_pinned_to_bottom) = create_signal(true);

    let height = height_class.unwrap_or("h-[50vh]".to_string());

    let update_pinned = move || {
        if let Some(el) = scroll_ref.get() {
            let el: HtmlElement = el.into();
            let scroll_top = el.scroll_top() as f64;
            let scroll_h = el.scroll_height() as f64;
            let client_h = el.client_height() as f64;

            // within 48px of bottom counts as "at bottom"
            let near_bottom = (scroll_h - (scroll_top + client_h)) <= 48.0;
            set_pinned_to_bottom.set(near_bottom);
        }
    };

    let scroll_to_bottom = move || {
        if let Some(el) = scroll_ref.get() {
            let el: HtmlElement = el.into();
            el.set_scroll_top(el.scroll_height());
        }
    };

    // When new content arrives, scroll only if user is pinned.
    create_effect(move |_| {
        // Track changes: using rows length avoids key-collision issues
        let _n = rows.get().len();
        if pinned_to_bottom.get() {
            request_animation_frame(move || scroll_to_bottom());
        }
    });

    view! {
        <div
            node_ref=scroll_ref
            class=format!("{} overflow-auto p-3", height)
            on:scroll=move |_| update_pinned()
        >
            <div class="space-y-1">
                <For
                    each=move || rows.get().into_iter().enumerate()
                    key=|(idx, row)| match row {
                        TranscriptRow::ToolExchange { call_id, .. } => format!("tool:{call_id}"),
                        TranscriptRow::Event(ev) => format!("ev:{}:{idx}", ev.ts_ms()),
                    }
                    children=move |(_idx, row): (usize, TranscriptRow)| {
                        view! { <TranscriptRowView row=row /> }
                    }
                />
            </div>
        </div>
    }
}

#[component]
pub fn TranscriptRowView(row: TranscriptRow) -> impl IntoView {
    match row {
        TranscriptRow::Event(ev) => match ev {
            TranscriptEvent::User(e) => view! { <UserBubble e=e /> }.into_any(),
            TranscriptEvent::Assistant(e) => view! { <AssistantBubble e=e /> }.into_any(),
            TranscriptEvent::CompactionSummary(e) => view! { <SummaryCard e=e /> }.into_any(),
            TranscriptEvent::CustomMessage(e) => view! { <CustomMessageCard e=e /> }.into_any(),
            TranscriptEvent::CustomNote(e) => view! { <CustomNoteCard e=e /> }.into_any(),
            TranscriptEvent::ToolCall(_) | TranscriptEvent::ToolResult(_) => {
                // Should not happen after grouping.
                ().into_any()
            }
        },
        TranscriptRow::ToolExchange {
            call_id,
            tool,
            call_ts_ms,
            call_args,
            result_ts_ms,
            ok,
            result_json,
            error,
        } => view! {
            <ToolExchangeRow
                call_id=call_id
                tool=tool
                call_ts_ms=call_ts_ms
                call_args=call_args
                result_ts_ms=result_ts_ms
                ok=ok
                result_json=result_json
                error=error
            />
        }
        .into_any(),
    }
}

// ---------------- chat bubbles ----------------

#[component]
pub fn UserBubble(e: UserEvent) -> impl IntoView {
    let ts = format_time(e.ts_ms);
    view! {
        <div class="flex justify-end items-end mt-3">
            <div class="flex flex-row-reverse items-end space-x-2 space-x-reverse max-w-[75%]">
                <div class="py-2 px-3 text-sm rounded-md shadow-sm bg-primary text-primary-foreground">
                    <p class="leading-snug whitespace-pre-wrap">{e.body}</p>
                    <p class="mt-1 text-right text-[10px] text-primary-foreground/70">{ts}</p>
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn AssistantBubble(e: AssistantEvent) -> impl IntoView {
    let ts = format_time(e.ts_ms);
    view! {
        <div class="flex justify-start items-end mt-3">
            <div class="flex items-end space-x-2 max-w-[75%]">
                <div class="py-2 px-3 text-sm rounded-md shadow-sm bg-muted">
                    <p class="leading-snug whitespace-pre-wrap">{e.body}</p>
                    <p class="mt-1 text-right text-[10px] text-muted-foreground/70">{ts}</p>
                </div>
            </div>
        </div>
    }
}

// ---------------- tool exchange (compact, expandable, no cards) ----------------

#[component]
pub fn ToolExchangeRow(
    call_id: String,
    tool: String,
    call_ts_ms: i64,
    call_args: Json,
    result_ts_ms: Option<i64>,
    ok: Option<bool>,
    result_json: Option<Json>,
    error: Option<String>,
) -> impl IntoView {
    let (open, set_open) = create_signal(false);

    let ts_call = format_time(call_ts_ms);
    let ts_res = result_ts_ms.map(format_time);

    let status = ok.map(|v| if v { "ok" } else { "error" });
    let status_class = match ok {
        Some(true) => "text-emerald-600 dark:text-emerald-300",
        Some(false) => "text-red-600 dark:text-red-300",
        None => "text-muted-foreground",
    };

    let args_preview = truncate_one_line(&pretty_json(&call_args), 160);
    let result_preview = result_json
        .as_ref()
        .map(|v| truncate_one_line(&pretty_json(v), 160));

    // Cloneables for repeated renders
    let args_preview_c = args_preview.clone();
    let result_preview_c = result_preview.clone();
    let error_c = error.clone();
    let result_json_c = result_json.clone();

    view! {
        <div class="mt-2 flex justify-start">
            <div class="max-w-[85%] w-full">
                <button
                    type="button"
                    class="w-full text-left"
                    on:click=move |_| set_open.update(|v| *v = !*v)
                >
                    <div class="flex items-center gap-2 py-1">
                        <span class="text-xs text-muted-foreground">
                            {move || {
                                if open.get() {
                                    view! { <ChevronDown class="size-4" /> }.into_any()
                                } else {
                                    view! { <ChevronRight class="size-4" /> }.into_any()
                                }
                            }}
                        </span>

                        <span class="text-xs font-semibold text-muted-foreground uppercase">"tool"</span>
                        <span class="text-xs font-mono text-foreground/80 truncate">{tool.clone()}</span>

                        //<span class="text-[10px] text-muted-foreground/70 ml-auto shrink-0">{ts_call}</span>

                        <span class=format!("text-xs font-medium ml-2 shrink-0 {}", status_class)>
                            {status.unwrap_or("pending")}
                        </span>

                        {move || {
                            ts_res.clone().map(|t| {
                                view! {
                                    <span class="text-xs text-muted-foreground/70 shrink-0">
                                        {format!("· {}", t)}
                                    </span>
                                }
                            })
                        }}
                    </div>

                    // <div class="pl-5 pb-1">
                    //     <div class="text-xs text-muted-foreground/70 font-mono truncate">
                    //         {format!("call_id={}", call_id.clone())}
                    //     </div>
                    // </div>
                </button>

                <Show
                    when=move || open.get()
                    fallback=|| ()
                >
                    {({
                        // create an Fn() child by returning a closure
                        let args_preview = args_preview_c.clone();
                        let result_preview = result_preview_c.clone();
                        let error = error_c.clone();
                        let result_json = result_json_c.clone();

                        move || view! {
                            <div class="pl-5 pb-2 space-y-2">
                                <div>
                                    <div class="text-xs text-muted-foreground">"args"</div>
                                    <pre class="mt-1 text-xs font-mono whitespace-pre-wrap break-words text-foreground/80">
                                        {args_preview.clone()}
                                    </pre>
                                </div>

                                {move || {
                                    if let Some(err) = error.clone().filter(|s| !s.is_empty()) {
                                        view! {
                                            <div>
                                                <div class="text-xs text-muted-foreground">"error"</div>
                                                <pre class="mt-1 text-xs font-mono whitespace-pre-wrap break-words text-red-700/80 dark:text-red-300/80">
                                                    {err}
                                                </pre>
                                            </div>
                                        }.into_any()
                                    } else {
                                        ().into_any()
                                    }
                                }}

                                {move || {
                                    if let Some(prev) = result_preview.clone() {
                                        view! {
                                            <div>
                                                <div class="text-xs text-muted-foreground">"result preview"</div>
                                                <pre class="mt-1 text-xs font-mono whitespace-pre-wrap break-words text-foreground/80">
                                                    {prev}
                                                </pre>
                                            </div>
                                        }.into_any()
                                    } else {
                                        ().into_any()
                                    }
                                }}

                                {move || {
                                    if let Some(v) = result_json.clone() {
                                        view! {
                                            <div>
                                                <div class="text-xs text-muted-foreground">"full result"</div>
                                                <JsonViewer value=v />
                                            </div>
                                        }.into_any()
                                    } else {
                                        ().into_any()
                                    }
                                }}
                            </div>
                        }
                    })()}
                </Show>
            </div>
        </div>
    }
}

#[component]
pub fn JsonViewer(value: Json) -> impl IntoView {
    let s = pretty_json(&value);
    view! {
        <pre class="mt-1 max-h-[320px] overflow-auto rounded-md bg-background/50 p-2 text-xs font-mono whitespace-pre-wrap break-words">
            {s}
        </pre>
    }
}

// ---------------- system-ish cards ----------------

#[component]
pub fn SummaryCard(e: CompactionSummaryEvent) -> impl IntoView {
    let ts = format_time(e.ts_ms);
    view! {
        <div class="mt-4 flex justify-center">
            <div class="max-w-[85%] w-full rounded-md border bg-secondary/40 px-3 py-2 shadow-sm">
                <div class="flex items-center justify-between">
                    <span class="text-xs font-semibold tracking-wide text-muted-foreground uppercase">"Summary"</span>
                    <span class="text-[10px] text-muted-foreground/70">{ts}</span>
                </div>
                <div class="mt-2 text-sm whitespace-pre-wrap">{e.summary}</div>
                <div class="mt-2 text-[10px] text-muted-foreground">
                    {format!("covers_up_to_ts_ms={}", e.covers_up_to_ts_ms)}
                </div>
            </div>
        </div>
    }
}

#[component]
pub fn CustomMessageCard(e: CustomMessageEvent) -> impl IntoView {
    let ts = format_time(e.ts_ms);
    view! {
        <div class="mt-3 flex justify-center">
            <div class="max-w-[85%] w-full rounded-md border bg-muted/10 px-3 py-2">
                <div class="flex items-center justify-between">
                    <span class="text-xs font-semibold tracking-wide text-muted-foreground uppercase">
                        {format!("message ({})", e.role)}
                    </span>
                    <span class="text-[10px] text-muted-foreground/70">{ts}</span>
                </div>
                <div class="mt-2 text-sm whitespace-pre-wrap">{e.body}</div>
            </div>
        </div>
    }
}

#[component]
pub fn CustomNoteCard(e: CustomNoteEvent) -> impl IntoView {
    let ts = format_time(e.ts_ms);
    let preview = truncate_one_line(&pretty_json(&e.value), 220);
    let (open, set_open) = create_signal(false);

    view! {
        <div class="mt-3 flex justify-center">
            <div class="max-w-[85%] w-full rounded-md border bg-muted/5 px-3 py-2">
                <button class="w-full text-left" on:click=move |_| set_open.update(|v| *v = !*v)>
                    <div class="flex items-center justify-between gap-3">
                        <div class="min-w-0">
                            <span class="text-xs font-semibold tracking-wide text-muted-foreground uppercase">"note"</span>
                            <span class="ml-2 text-xs font-mono text-foreground/80 truncate">{e.key.clone()}</span>
                        </div>
                        <div class="flex items-center gap-2 shrink-0">
                            <span class="text-[10px] text-muted-foreground/70">{ts}</span>
                            <span class="text-[10px] text-muted-foreground">{move || if open.get() { "▲" } else { "▼" }}</span>
                        </div>
                    </div>

                    <div class="mt-2 text-xs text-muted-foreground">"preview"</div>
                    <pre class="mt-1 text-xs font-mono whitespace-pre-wrap break-words text-foreground/70">{preview.clone()}</pre>
                </button>

                <Show when=move || open.get() fallback=|| ()>
                    <div class="mt-3 border-t pt-3">
                        <div class="text-xs text-muted-foreground">"full value"</div>
                        <JsonViewer value=e.value.clone() />
                    </div>
                </Show>
            </div>
        </div>
    }
}
