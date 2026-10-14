//! Who is signed in, as far as this device knows.
//!
//! The app starts from the account stored in IndexedDB (so it renders
//! instantly and offline), then confirms with `GET /api/me`.

use gloo_net::http::Request;
use leptos::{logging, prelude::*, task::spawn_local};
use prost::Message as _;
use supp_shared::proto::api::v1::Me;

use crate::{
    pwa::{self, NewVersion},
    repo::Repo,
    sync::SyncStatus,
};

#[derive(Clone, Debug, PartialEq)]
pub enum Session {
    /// Never signed in on this device: local only, no syncing.
    Anonymous,
    /// Signed in (or, while offline, last known to be).
    SignedIn(Me),
    /// The local lists belong to this account, but its session has ended.
    Expired(Me),
    /// `current` is signed in, but the local lists belong to `previous`.
    Mismatch { previous: Me, current: Me },
}

/// Whether the server has been asked who is signed in (or couldn't be
/// reached). Until then a fresh device looks `Anonymous` even when it has
/// just signed in.
#[derive(Clone, Copy)]
pub struct SessionChecked(pub RwSignal<bool>);

impl Session {
    pub fn from_stored(stored: Option<Me>) -> Self {
        stored.map_or(Self::Anonymous, Self::SignedIn)
    }
}

/// Asks the server who is signed in: `Ok(None)` means nobody, `Err` means
/// the server could not be reached.
pub async fn fetch_me() -> Result<Option<Me>, String> {
    let response = Request::get("/api/me")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    match response.status() {
        200 => {
            let bytes = response.binary().await.map_err(|e| e.to_string())?;
            Me::decode(bytes.as_slice())
                .map(Some)
                .map_err(|e| e.to_string())
        }
        401 => Ok(None),
        status => Err(format!("/api/me returned {status}")),
    }
}

/// Reconciles the stored account with the server's answer.
pub async fn resolve(repo: Repo, session: RwSignal<Session>) {
    let stored = match repo.account().await {
        Ok(stored) => stored,
        Err(err) => {
            logging::error!("reading account: {err}");
            return;
        }
    };
    let resolved = match (fetch_me().await, stored) {
        // Offline: keep believing the stored account.
        (Err(_), stored) => Session::from_stored(stored),
        (Ok(None), None) => Session::Anonymous,
        (Ok(None), Some(previous)) => Session::Expired(previous),
        // First sign-in on this device: the anonymous lists become this
        // account's and upload when syncing starts.
        (Ok(Some(me)), None) => adopt(repo, me).await,
        (Ok(Some(me)), Some(previous)) if previous.user_id == me.user_id => adopt(repo, me).await,
        (Ok(Some(current)), Some(previous)) => Session::Mismatch { previous, current },
    };
    session.set(resolved);
}

/// The server says nobody is signed in: a `SignedIn` session has ended.
pub fn expire(session: RwSignal<Session>) {
    session.update(|s| {
        if let Session::SignedIn(me) = s {
            *s = Session::Expired(me.clone());
        }
    });
}

/// Makes `me` the owner of the local lists.
pub async fn adopt(repo: Repo, me: Me) -> Session {
    if let Err(err) = repo.set_account(&me).await {
        logging::error!("saving account: {err}");
    }
    Session::SignedIn(me)
}

/// Ends the session, then erases this device's data. Needs the network:
/// erasing local data while the server session lives on would leave the
/// browser signed in.
pub async fn sign_out(repo: Repo) -> Result<(), String> {
    let response = Request::post("/auth/logout")
        .send()
        .await
        .map_err(|_| "You need to be online to sign out.".to_string())?;
    if !response.ok() {
        return Err(format!("Sign-out failed ({}).", response.status()));
    }
    repo.erase_all().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[component]
pub fn AccountBar() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let session = expect_context::<RwSignal<Session>>();
    let status = expect_context::<RwSignal<SyncStatus>>();
    let new_version = expect_context::<NewVersion>().0;
    let (problem, set_problem) = signal(None::<String>);
    let (updating, set_updating) = signal(false);
    let status_text = move || match status.get() {
        SyncStatus::Idle => "",
        SyncStatus::Connecting => "Connecting…",
        SyncStatus::Online => "Synced",
        SyncStatus::Offline => "Offline: changes are saved on this device",
        SyncStatus::UpgradeRequired => "",
    };

    let do_sign_out = move |_| {
        spawn_local(async move {
            match sign_out(repo).await {
                Ok(()) => {
                    let _ = window().location().assign("/");
                }
                Err(err) => set_problem.set(Some(err)),
            }
        })
    };
    let keep = move |current: Me| {
        move |_| {
            let current = current.clone();
            spawn_local(async move { session.set(adopt(repo, current).await) })
        }
    };
    let erase = move |current: Me| {
        move |_| {
            let current = current.clone();
            spawn_local(async move {
                if let Err(err) = repo.erase_all().await {
                    set_problem.set(Some(err.to_string()));
                    return;
                }
                session.set(adopt(repo, current).await);
            })
        }
    };

    view! {
        <nav class="account">
            {move || match session.get() {
                Session::Anonymous => view! {
                    <span class="muted">"Lists are stored on this device only."</span>
                    <a href="/auth/login" rel="external">"Sign in to sync"</a>
                }
                .into_any(),
                Session::SignedIn(me) => view! {
                    <span class="sync-status" data-status=move || format!("{:?}", status.get())>
                        {status_text}
                    </span>
                    <span class="who">{me.name}</span>
                    <button class="link" on:click=do_sign_out>"Sign out"</button>
                }
                .into_any(),
                Session::Expired(me) => view! {
                    <span class="banner">
                        "Signed out of " {me.email} ". Your changes are kept on this device."
                    </span>
                    <a href="/auth/login" rel="external">"Sign in to resume syncing"</a>
                }
                .into_any(),
                Session::Mismatch { previous, current } => view! {
                    <span class="banner">
                        "This device has lists from " {previous.email.clone()}
                        ". You are signed in as " {current.email.clone()} "."
                    </span>
                    <button on:click=keep(current.clone())>
                        "Add them to " {current.email.clone()}
                    </button>
                    <button class="danger" on:click=erase(current)>"Erase them"</button>
                }
                .into_any(),
            }}
            // Offered when a new version has installed, or when the server
            // refuses this one.
            {move || {
                (new_version.get() || status.get() == SyncStatus::UpgradeRequired)
                    .then(|| {
                        view! {
                            <span class="banner">"A new version of the app is available."</span>
                            <button
                                disabled=updating
                                on:click=move |_| {
                                    set_updating.set(true);
                                    spawn_local(pwa::update_and_reload());
                                }
                            >
                                {move || if updating.get() { "Updating…" } else { "Reload" }}
                            </button>
                        }
                    })
            }}
            {move || problem.get().map(|p| view! { <span class="banner error">{p}</span> })}
        </nav>
    }
}
