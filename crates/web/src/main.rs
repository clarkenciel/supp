mod list_page;
mod overview;
mod repo;
mod storage;

use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::{list_page::ListPage, overview::Overview, repo::Repo};

fn main() {
    console_error_panic_hook::set_once();
    // Leptos's executor is set up by mounting, which waits for storage, so
    // the bootstrap future runs on wasm-bindgen's directly.
    wasm_bindgen_futures::spawn_local(async {
        match Repo::open().await {
            Ok(repo) => leptos::mount::mount_to_body(move || view! { <App repo /> }),
            Err(err) => {
                let err = err.to_string();
                leptos::mount::mount_to_body(move || {
                    view! {
                        <main>
                            <p class="status error">"Could not open local storage: " {err}</p>
                        </main>
                    }
                })
            }
        }
    });
}

#[component]
fn App(repo: Repo) -> impl IntoView {
    provide_context(repo);
    view! {
        <main>
            <Router>
                <Routes fallback=|| view! { <p class="status">"Not found."</p> }>
                    <Route path=path!("/") view=Overview />
                    <Route path=path!("/lists/:id") view=ListPage />
                </Routes>
            </Router>
        </main>
    }
}
