//! Who is signed in, as far as this device knows.
//!
//! The app starts from the account stored in IndexedDB (so it renders
//! instantly and offline), then confirms with `GET /api/me`.

use gloo_net::http::Request;
use leptos::{logging, prelude::*, task::spawn_local};
use prost::Message as _;
use supp_shared::proto::api::v1::Me;

use supp_ui::{ErrorStatus, Notice};

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

/// Another tab has stored `user` as this device's account (`None`: signed
/// out). If this tab's session doesn't match, it reloads.
///
/// Reloading, rather than patching this tab's state, because an account
/// change is rare and touches everything a tab derives at startup from
/// storage and `/api/me`: the session, the sync loop's connection, and,
/// after signing out or settling a mismatch by erasing, every list. A fresh
/// start can't keep anything stale. It can't loop either: the message is
/// sent only on adopting the account `/api/me` reports or on signing out,
/// tabs share the session cookie, so a reloaded tab that adopts agrees with
/// the sender, and tabs that agree ignore the message.
pub fn account_changed_elsewhere(session: RwSignal<Session>, user: Option<i64>) {
    let agrees = session.with_untracked(|s| match s {
        Session::Anonymous => user.is_none(),
        Session::SignedIn(me) => user == Some(me.user_id),
        // Signed in again, signed out, or the mismatch was settled.
        Session::Expired(_) | Session::Mismatch { .. } => false,
    });
    if !agrees {
        let _ = window().location().reload();
    }
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
    repo.announce_signed_out();
    Ok(())
}

/// What the account icon's badge says, most urgent first.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Badge {
    /// Not signed in: the icon is the device itself.
    Local,
    /// Something needs the person: a notice under the header says what.
    Attention,
    Offline,
    Connecting,
    Synced,
    None,
}

/// The account icon at the end of every page header: who is signed in and
/// whether their lists are syncing, in one glyph. Tapping it opens the
/// details and the sign-in or sign-out action.
#[component]
pub fn AccountMenu() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let session = expect_context::<RwSignal<Session>>();
    let status = expect_context::<RwSignal<SyncStatus>>();
    let new_version = expect_context::<NewVersion>().0;
    let (open, set_open) = signal(false);
    let on_key = window_event_listener(leptos::ev::keydown, move |ev| {
        if ev.key() == "Escape" {
            set_open.set(false);
        }
    });
    on_cleanup(move || on_key.remove());

    let badge = Memo::new(move |_| {
        let attention = new_version.get() || repo.storage_closed().get();
        match session.get() {
            Session::Anonymous if attention => Badge::Attention,
            Session::Anonymous => Badge::Local,
            Session::Expired(_) | Session::Mismatch { .. } => Badge::Attention,
            Session::SignedIn(_) => match status.get() {
                _ if attention => Badge::Attention,
                SyncStatus::UpgradeRequired => Badge::Attention,
                SyncStatus::Offline => Badge::Offline,
                SyncStatus::Connecting => Badge::Connecting,
                SyncStatus::Online => Badge::Synced,
                SyncStatus::Idle => Badge::None,
            },
        }
    });
    let label = move || {
        let state = match badge.get() {
            Badge::Local => "not signed in",
            Badge::Attention => "needs attention",
            Badge::Offline => "offline",
            Badge::Connecting => "connecting",
            Badge::Synced => "synced",
            Badge::None => "signed in",
        };
        format!("Account: {state}")
    };
    let initial = Signal::derive(move || match session.get() {
        Session::SignedIn(me) | Session::Expired(me) | Session::Mismatch { current: me, .. } => me
            .name
            .chars()
            .next()
            .map(|c| c.to_uppercase().to_string())
            .unwrap_or_default(),
        Session::Anonymous => String::new(),
    });

    view! {
        <div class="account">
            <button
                class="account-icon"
                aria-label=label
                aria-expanded=move || open.get().to_string()
                aria-controls="account-panel"
                on:click=move |_| set_open.update(|o| *o = !*o)
            >
                <AccountGlyph badge=badge.into() initial />
            </button>
            <Show when=move || open.get()>
                <div class="account-scrim" on:click=move |_| set_open.set(false)></div>
            </Show>
            <AccountPanel open />
        </div>
    }
}

/// The account icon's drawing: an avatar ring with the person's initial
/// (a phone outline when nobody is signed in), and the badge for `badge`.
#[component]
fn AccountGlyph(badge: Signal<Badge>, initial: Signal<String>) -> impl IntoView {
    view! {
        <svg viewBox="0 0 40 40" aria-hidden="true" data-badge=move || format!("{:?}", badge.get())>
            {move || match badge.get() {
                Badge::Local => view! {
                    <rect class="ink" x="13" y="8" width="14" height="24" rx="3" />
                    <path class="ink" d="M18 28h4" />
                }
                .into_any(),
                b => view! {
                    <circle class="ink" class:dashed=b == Badge::Connecting cx="18" cy="18" r="14" />
                    <text x="18" y="23" text-anchor="middle">{initial}</text>
                    {badge_mark(b)}
                }
                .into_any(),
            }}
        </svg>
    }
}

/// What the account icon opens onto: who is signed in, how syncing is
/// going in words, and signing in or out.
#[component]
fn AccountPanel(open: ReadSignal<bool>) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let session = expect_context::<RwSignal<Session>>();
    let status = expect_context::<RwSignal<SyncStatus>>();
    let (problem, set_problem) = signal(None::<String>);
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

    view! {
        <div id="account-panel" class="account-panel" hidden=move || !open.get()>
            {move || match session.get() {
                Session::Anonymous => view! {
                    <p class="muted">"Lists are stored on this device only."</p>
                    <a class="button secondary" href="/auth/login" rel="external">
                        "Sign in to sync"
                    </a>
                }
                .into_any(),
                Session::SignedIn(me) => view! {
                    <p class="who">{me.name}</p>
                    <p class="sync-status" data-status=move || format!("{:?}", status.get())>
                        {status_text}
                    </p>
                    <button class="secondary" on:click=do_sign_out>"Sign out"</button>
                }
                .into_any(),
                Session::Expired(me) | Session::Mismatch { current: me, .. } => view! {
                    <p class="who">{me.name}</p>
                    <p class="muted">"See the note under the header."</p>
                }
                .into_any(),
            }}
            {move || problem.get().map(|p| view! { <ErrorStatus>{p}</ErrorStatus> })}
        </div>
    }
}

/// The small mark at the icon's lower right, if the state has one.
fn badge_mark(badge: Badge) -> impl IntoView {
    match badge {
        // A cloud with a tick: what is OK, not just that something is.
        Badge::Synced => view! {
            <circle class="halo" cx="31" cy="31" r="8.5" />
            <path
                class="cloud"
                d="M26 35h9.5a3.6 3.6 0 0 0 .4-7.2 5.3 5.3 0 0 0-10.1 1.1A3.1 3.1 0 0 0 26 35z"
            />
            <path class="tick" d="M28.6 31.4l1.6 1.6 3-3" />
        }
        .into_any(),
        Badge::Offline => view! {
            <circle class="halo" cx="31" cy="31" r="8.5" />
            <circle class="offline" cx="31" cy="31" r="6.5" />
            <path class="offline" d="M27 35l8-8" />
        }
        .into_any(),
        Badge::Attention => view! {
            <path class="halo" d="M31 22l8 14H23z" />
            <path class="warn" d="M31 23.5l7 12h-14z" />
            <path class="warn-mark" d="M31 28v3" />
            <circle class="warn-dot" cx="31" cy="33.6" r="1" />
        }
        .into_any(),
        Badge::Local | Badge::Connecting | Badge::None => ().into_any(),
    }
}

/// Notes that need the person's attention, under the page header: an
/// ended session, someone else's lists on this device, a new version.
#[component]
pub fn Notices() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let session = expect_context::<RwSignal<Session>>();
    let status = expect_context::<RwSignal<SyncStatus>>();
    let new_version = expect_context::<NewVersion>().0;
    let (problem, set_problem) = signal(None::<String>);
    let (updating, set_updating) = signal(false);

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
        <div class="notices">
            {move || match session.get() {
                Session::Expired(me) => Some(
                    view! {
                        <Notice>
                            <p>
                                "Signed out of " {me.email} ". Your changes are kept on this device."
                            </p>
                            <a class="button secondary" href="/auth/login" rel="external">
                                "Sign in to resume syncing"
                            </a>
                        </Notice>
                    }
                    .into_any(),
                ),
                Session::Mismatch { previous, current } => Some(
                    view! {
                        <Notice>
                            <p>
                                "This device has lists from " {previous.email.clone()}
                                ". You are signed in as " {current.email.clone()} "."
                            </p>
                            <div class="actions">
                                <button on:click=keep(current.clone())>
                                    "Add them to " {current.email.clone()}
                                </button>
                                <button class="danger" on:click=erase(current)>"Erase them"</button>
                            </div>
                        </Notice>
                    }
                    .into_any(),
                ),
                Session::Anonymous | Session::SignedIn(_) => None,
            }}
            // Offered when a new version has installed, or when the server
            // refuses this one, or another tab runs one that needed this
            // tab to let go of the database.
            {move || {
                (new_version.get() || status.get() == SyncStatus::UpgradeRequired
                    || repo.storage_closed().get())
                    .then(|| {
                        let text = if repo.storage_closed().get() {
                            "A new version of the app is open in another tab. \
                             Reload to keep saving changes here."
                        } else {
                            "A new version of the app is available."
                        };
                        view! {
                            <Notice>
                                <p>{text}</p>
                                <button
                                    disabled=updating
                                    on:click=move |_| {
                                        set_updating.set(true);
                                        spawn_local(pwa::update_and_reload());
                                    }
                                >
                                    {move || if updating.get() { "Updating…" } else { "Reload" }}
                                </button>
                            </Notice>
                        }
                    })
            }}
            {move || problem.get().map(|p| view! { <ErrorStatus>{p}</ErrorStatus> })}
        </div>
    }
}

/// A page's title bar: whatever the page puts first, then the account
/// icon, with any notices underneath.
#[component]
pub fn PageHeader(children: Children) -> impl IntoView {
    view! {
        <header>
            {children()}
            <AccountMenu />
        </header>
        <Notices />
    }
}
