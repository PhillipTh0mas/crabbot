use leptos::prelude::*;

use icons::History;

use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};

#[component]
pub fn SessionCell(
    session_key: String,
    /// One of: "user", "thinking", "task", "tool", "unknown"
    #[prop(optional, into)]
    session_type: String,
    /// Human-readable label
    #[prop(optional, into)]
    label: String,
    html: Signal<String>,
    on_open_history: Callback<String>,
    #[prop(optional)] running: bool,
) -> impl IntoView {
    let sk_for_card = session_key.clone();
    let sk_for_btn = session_key.clone();
    let display_label = if label.is_empty() {
        session_key.clone()
    } else {
        label
    };

    let type_tag = session_type.clone();

    let (badge_text, badge_class) = match type_tag.as_str() {
        "thinking" => (
            "Thinking",
            "bg-violet-500/15 text-violet-700 dark:text-violet-400 border-violet-500/20",
        ),
        "task" => (
            "Task",
            "bg-amber-500/15 text-amber-700 dark:text-amber-400 border-amber-500/20",
        ),
        "tool" => (
            "Tool",
            "bg-cyan-500/15 text-cyan-700 dark:text-cyan-400 border-cyan-500/20",
        ),
        "user" => (
            "Chat",
            "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400 border-emerald-500/20",
        ),
        _ => (
            "Other",
            "bg-zinc-500/15 text-zinc-600 dark:text-zinc-400 border-zinc-500/20",
        ),
    };

    view! {
        <div
            class="relative rounded-xl bg-card border p-3 hover:border-foreground/20 transition-colors cursor-pointer"
            on:click=move |_| on_open_history.run(sk_for_card.clone())
        >
            <div class="mb-2 flex items-start justify-between gap-2">
                <div class="min-w-0 flex-1">
                    <div class="flex items-center gap-2 mb-0.5">
                        <span class=format!(
                            "inline-flex items-center rounded-md border px-1.5 py-0.5 text-[10px] font-semibold leading-none {}",
                            badge_class
                        )>
                            {badge_text}
                        </span>
                        {if running {
                            view! {
                                <span class="inline-flex items-center gap-1 rounded-md border border-green-500/20 bg-green-500/15 px-1.5 py-0.5 text-[10px] font-semibold leading-none text-green-700 dark:text-green-400">
                                    <span class="relative flex h-1.5 w-1.5">
                                        <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-green-400 opacity-75"></span>
                                        <span class="relative inline-flex h-1.5 w-1.5 rounded-full bg-green-500"></span>
                                    </span>
                                    "Running"
                                </span>
                            }.into_any()
                        } else {
                            view! { <span></span> }.into_any()
                        }}
                    </div>
                    <div class="truncate text-xs font-semibold text-foreground">{display_label}</div>
                    <div class="truncate text-[10px] text-muted-foreground font-mono mt-0.5">{session_key}</div>
                </div>

                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Icon
                    class="h-7 w-7 shrink-0"
                    attr:title="View History"
                    on:click=move |e: leptos::ev::MouseEvent| {
                        e.stop_propagation();
                        on_open_history.run(sk_for_btn.clone());
                    }
                >
                    <History class="h-3.5 w-3.5 text-muted-foreground" />
                    <span class="hidden">"History"</span>
                </Button>
            </div>

            <div class="prose prose-sm dark:prose-invert max-w-none text-xs" inner_html=move || html.get() />
        </div>
    }
}
