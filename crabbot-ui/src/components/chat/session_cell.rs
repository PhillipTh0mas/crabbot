use leptos::prelude::*;

use icons::History;

use crate::components::ui::button::{Button, ButtonSize, ButtonVariant};

#[component]
pub fn SessionCell(
    session_key: String,
    html: Signal<String>,
    on_open_history: Callback<String>,
) -> impl IntoView {
    let sk_for_btn = session_key.clone();

    view! {
        <div class="relative bg-background border p-3">
            <div class="mb-2 flex items-start justify-between gap-2">
                <div class="min-w-0">
                    <div class="truncate text-xs font-semibold">{session_key}</div>
                </div>

                <Button
                    variant=ButtonVariant::Ghost
                    size=ButtonSize::Icon
                    class="h-8 w-8"
                    on:click=move |_| on_open_history.run(sk_for_btn.clone())
                >
                    <History class="h-4 w-4 text-muted-foreground" />
                    <span class="hidden">"History"</span>
                </Button>
            </div>

            <div class="prose prose-sm dark:prose-invert max-w-none" inner_html=move || html.get() />
        </div>
    }
}
