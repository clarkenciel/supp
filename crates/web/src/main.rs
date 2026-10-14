mod list_page;
mod overview;
mod pwa;
mod repo;
mod session;
mod share;
mod storage;
mod sync;
mod tabs;

use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use crate::{
    list_page::ListPage,
    overview::Overview,
    pwa::NewVersion,
    repo::Repo,
    session::{AccountBar, Session, SessionChecked},
    sync::SyncStatus,
};

fn main() {
    console_error_panic_hook::set_once();
    wasm_bindgen_futures::spawn_local(pwa::register());
    wasm_bindgen_futures::spawn_local(storage::request_persistence());
    // Leptos's executor is set up by mounting, which waits for storage, so
    // the bootstrap future runs on wasm-bindgen's directly.
    wasm_bindgen_futures::spawn_local(async {
        match Repo::open().await {
            Ok(repo) => {
                // Before the sync loop exists, so it can't send the leave.
                if let Some(id) = sync::take_joined_list()
                    && let Err(err) = repo.forget_left(id).await
                {
                    leptos::logging::error!("dropping the pending leave of {id}: {err}");
                }
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
    let new_version = NewVersion(RwSignal::new(false));
    let checked = SessionChecked(RwSignal::new(false));
    provide_context(repo);
    provide_context(session);
    provide_context(status);
    provide_context(new_version);
    provide_context(checked);
    leptos::task::spawn_local(pwa::watch_for_updates(new_version.0));
    leptos::task::spawn_local(async move {
        session::resolve(repo, session).await;
        checked.0.set(true);
    });
    repo.follow_other_tabs();
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
