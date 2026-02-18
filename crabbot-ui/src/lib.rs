mod components;
use leptos::prelude::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    mount_to_body(|| view! { <App/> });
}

#[component]
fn App() -> impl IntoView {
    components::chat::connected::ChatUiConnected()
}
