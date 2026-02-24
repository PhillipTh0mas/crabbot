use crate::components::transcript::TranscriptList;
use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};
use icons::{History, Send};
use leptos::html;
use leptos::prelude::*;
use web_sys::HtmlElement;

use crabbot_shared::api::transcript::TranscriptEvent;

#[component]
pub fn ChatTopBar(
    main_session: ReadSignal<Option<String>>,
    draft: ReadSignal<String>,
    set_draft: WriteSignal<String>,
    history_open: ReadSignal<bool>,
    set_history_open: WriteSignal<bool>,
    on_send: Callback<()>,
    on_new_chat: Callback<()>,
    history_events: Signal<Vec<TranscriptEvent>>,
) -> impl IntoView {
    let send_disabled =
        Signal::derive(move || main_session.get().is_none() || draft.get().trim().is_empty());

    view! {
        <div
            class="text-foreground"
            tabindex="-1"
            on:focusin=move |_| set_history_open.set(true)
            on:focusout=move |e: leptos::ev::FocusEvent| {
                // Only close if focus moved completely outside this wrapper
                if e.related_target().is_none() {
                    set_history_open.set(false);
                }
            }
        >
            <div class="mx-auto flex max-w-5xl items-center gap-2 px-4 py-3">
                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Icon
                    class="h-10 w-10"
                    on:click=move |_| set_history_open.update(|v| *v = !*v)
                >
                    <History class="h-5 w-5 text-muted-foreground" />
                </Button>

                <form
                    class="flex flex-1 items-center gap-2"
                    on:submit=move |ev| { ev.prevent_default(); on_send.run(()); }
                >
                    <input
                        class="h-12 flex-1 rounded-2xl border bg-background px-4 text-base outline-none focus:ring-2 focus:ring-ring/50"
                        prop:value=move || draft.get()
                        on:input=move |e| set_draft.set(event_target_value(&e))
                        on:keydown=move |e: leptos::ev::KeyboardEvent| {
                            if e.key() == "Enter" && !e.shift_key() {
                                e.prevent_default();
                                on_send.run(());
                            }
                        }
                        placeholder="talk to me"
                        autocomplete="off"
                        disabled=move || main_session.get().is_none()
                    />

                    <button
                        type="button"
                        class="inline-flex h-12 w-12 items-center justify-center rounded-2xl bg-primary text-primary-foreground disabled:opacity-50"
                        on:click=move |_| on_send.run(())
                        disabled=move || send_disabled.get()
                    >
                        <Send class="h-4 w-4" />
                    </button>

                    <button
                        type="button"
                        class="inline-flex h-12 w-12 items-center justify-center rounded-2xl hover:bg-accent"
                        on:click=move |_| on_new_chat.run(())
                    >
                        <span class="text-sm font-semibold">"+"</span>
                    </button>
                </form>
            </div>

            <Show when=move || history_open.get() fallback=|| ()>
                <div class="mx-auto max-w-5xl px-4 pt-2">
                    <div class="rounded-2xl border bg-background/90 backdrop-blur shadow-xl text-foreground">
                        <div class="flex items-center justify-between border-b px-4 py-3">
                            <div class="text-xs font-medium text-muted-foreground">
                                {move || format!("History: {}", main_session.get().unwrap_or_else(|| "-".into()))}
                            </div>
                            <button
                                type="button"
                                class="h-8 w-8 rounded-md hover:bg-accent"
                                on:click=move |_| set_history_open.set(false)
                            >
                                "×"
                            </button>
                        </div>
                        <TranscriptList events=history_events height_class="h-[50vh]".to_string() />
                    </div>
                </div>
            </Show>
        </div>
    }
}
