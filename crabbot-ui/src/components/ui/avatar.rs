use leptos::prelude::*;
use leptos_ui::clx;
use tw_merge::tw_merge;

mod components {
    use super::*;
    clx! {Avatar, div, "relative flex size-8 shrink-0 overflow-hidden rounded-full"}
    clx! {AvatarFallback, div, "bg-muted absolute inset-0 flex size-full items-center justify-center rounded-full"}
}

pub use components::*;

#[component]
pub fn AvatarImage(#[prop(into, optional)] class: String) -> impl IntoView {
    let merged_class = tw_merge!("absolute inset-0 aspect-square size-full z-10", class);
    let node_ref = NodeRef::<leptos::html::Img>::new();

    view! {
        <img
            node_ref=node_ref
            class=merged_class
            on:error=move |_| {
                if let Some(img) = node_ref.get() {
                    let _ = img.set_attribute("style", "display: none;");
                }
            }
        />
    }
}