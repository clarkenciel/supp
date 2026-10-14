mod list_page;
mod overview;
mod repo;
mod session;
mod storage;
mod sync;

use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::{
    list_page::ListPage,
    overview::Overview,
    repo::Repo,
    session::{AccountBar, Session},
    sync::SyncStatus,
};

fn main() {
    console_error_panic_hook::set_once();
    // Leptos's executor is set up by mounting, which waits for storage, so
    // the bootstrap future runs on wasm-bindgen's directly.
    wasm_bindgen_futures::spawn_local(async {
        match Repo::open().await {
            Ok(repo) => {
                let stored = repo.account().await.ok().flatten();
                leptos::mount::mount_to_body(move || {
                    view! { <App repo session=Session::from_stored(stored) /> }
                })
            }
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
fn App(repo: Repo, session: Session) -> impl IntoView {
    let session = RwSignal::new(session);
    let status = RwSignal::new(SyncStatus::Idle);
    provide_context(repo);
    provide_context(session);
    provide_context(status);
    leptos::task::spawn_local(session::resolve(repo, session));
    sync::run_while_signed_in(repo, session, status);
    view! {
        <main>
            <AccountBar />
            <Router>
                <Routes fallback=|| view! { <p class="status">"Not found."</p> }>
                    <Route path=path!("/") view=Overview />
                    <Route path=path!("/lists/:id") view=ListPage />
                </Routes>
            </Router>
        </main>
    }
}
