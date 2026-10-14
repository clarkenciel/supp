//! Sharing a list by link. Needs an account and the network: links are
//! made by the server (`/api/lists/{id}/invites`), which shows each link's
//! secret only once, when it is created.

use gloo_net::http::{Request, Response};
use leptos::{ev::SubmitEvent, html, prelude::*, task::spawn_local};
use prost::Message;
use supp_shared::{
    DocId,
    proto::api::v1::{CreateShareLink, CreatedShareLink, Me, ShareLink, ShareLinks},
};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::session::{Session, expire};

const PROTOBUF: &str = "application/x-protobuf";

enum ApiError {
    Offline,
    SignedOut,
    Failed(String),
}

impl ApiError {
    fn message(&self, action: &str) -> String {
        match self {
            Self::Offline => format!("You're offline. You need a connection to {action}."),
            Self::SignedOut => "You've been signed out. Sign in again to share.".into(),
            Self::Failed(why) => format!("Couldn't {action}: {why}"),
        }
    }
}

fn invites_url(id: DocId) -> String {
    format!("/api/lists/{id}/invites")
}

async fn check(response: Result<Response, gloo_net::Error>) -> Result<Response, ApiError> {
    let response = response.map_err(|_| ApiError::Offline)?;
    match response.status() {
        200..=299 => Ok(response),
        401 => Err(ApiError::SignedOut),
        404 => Err(ApiError::Failed("this list isn't in your account.".into())),
        status => Err(ApiError::Failed(format!("the server said {status}."))),
    }
}

async fn decode<T: Message + Default>(response: Response) -> Result<T, ApiError> {
    let bytes = response.binary().await.map_err(|_| ApiError::Offline)?;
    T::decode(bytes.as_slice()).map_err(|e| ApiError::Failed(e.to_string()))
}

async fn create_link(id: DocId, request: CreateShareLink) -> Result<String, ApiError> {
    let body = js_sys::Uint8Array::from(request.encode_to_vec().as_slice());
    let request = Request::post(&invites_url(id))
        .header("Content-Type", PROTOBUF)
        .body(body)
        .map_err(|e| ApiError::Failed(e.to_string()))?;
    let response = check(request.send().await).await?;
    Ok(decode::<CreatedShareLink>(response).await?.url)
}

async fn fetch_links(id: DocId) -> Result<Vec<ShareLink>, ApiError> {
    let response = check(Request::get(&invites_url(id)).send().await).await?;
    Ok(decode::<ShareLinks>(response).await?.links)
}

async fn revoke_link(id: DocId, link: i64) -> Result<(), ApiError> {
    let url = format!("{}/{link}", invites_url(id));
    check(Request::delete(&url).send().await).await?;
    Ok(())
}

/// Copies with the Clipboard API, which is missing outside secure contexts
/// and may be refused.
async fn copy_to_clipboard(text: &str) -> Result<(), JsValue> {
    let navigator = window().navigator();
    let clipboard = js_sys::Reflect::get(&navigator, &"clipboard".into())?;
    if clipboard.is_undefined() {
        return Err(JsValue::UNDEFINED);
    }
    let clipboard: web_sys::Clipboard = clipboard.unchecked_into();
    JsFuture::from(clipboard.write_text(text)).await?;
    Ok(())
}

/// The share panel under a list's header.
#[component]
pub fn SharePanel(id: DocId) -> impl IntoView {
    let session = expect_context::<RwSignal<Session>>();
    let signed_in = Memo::new(move |_| match session.get() {
        Session::SignedIn(me) => Some(me),
        _ => None,
    });
    view! {
        <section class="share" aria-label="Share this list">
            {move || match signed_in.get() {
                Some(me) => view! { <ShareLinksPanel id me /> }.into_any(),
                None => match session.get() {
                    Session::Mismatch { .. } => view! {
                        <p class="hint">
                            "Choose what to do with this device's lists (above) before sharing."
                        </p>
                    }
                    .into_any(),
                    Session::Expired(_) => view! {
                        <p class="hint">
                            "You've been signed out. "
                            <a href="/auth/login" rel="external">"Sign in again to share"</a>
                        </p>
                    }
                    .into_any(),
                    _ => view! {
                        <p class="hint">
                            "Sharing needs an account, so this list can sync between "
                            "you and the people you share it with. "
                            <a href="/auth/login" rel="external">"Sign in to share"</a>
                        </p>
                    }
                    .into_any(),
                },
            }}
        </section>
    }
}

#[component]
fn ShareLinksPanel(id: DocId, me: Me) -> impl IntoView {
    let session = expect_context::<RwSignal<Session>>();
    let (uses, set_uses) = signal(1u32);
    let (days, set_days) = signal(7u32);
    let (busy, set_busy) = signal(false);
    let (created, set_created) = signal(None::<String>);
    let (copy_state, set_copy_state) = signal(None::<&'static str>);
    // None while loading.
    let (links, set_links) = signal(None::<Vec<ShareLink>>);
    let (error, set_error) = signal(None::<String>);
    let link_input = NodeRef::<html::Input>::new();

    let fail = move |err: ApiError, action: &str| {
        if let ApiError::SignedOut = err {
            expire(session);
        }
        set_error.set(Some(err.message(action)));
    };
    let refresh = move || {
        spawn_local(async move {
            match fetch_links(id).await {
                Ok(fetched) => set_links.set(Some(fetched)),
                Err(err) => fail(err, "load this list's links"),
            }
        })
    };
    refresh();

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        set_busy.set(true);
        set_error.set(None);
        let request = CreateShareLink {
            max_uses: uses.get_untracked(),
            expires_in_secs: days.get_untracked() * 24 * 60 * 60,
        };
        spawn_local(async move {
            match create_link(id, request).await {
                Ok(url) => {
                    set_created.set(Some(url));
                    set_copy_state.set(None);
                    refresh();
                }
                Err(err) => fail(err, "create a link"),
            }
            set_busy.set(false);
        });
    };
    let copy = move |_| {
        let Some(url) = created.get_untracked() else {
            return;
        };
        spawn_local(async move {
            if copy_to_clipboard(&url).await.is_ok() {
                set_copy_state.set(Some("Copied"));
            } else {
                // Leave it selected for the user to copy by hand (unless
                // the panel closed meanwhile).
                if let Some(input) = link_input.try_get_untracked().flatten() {
                    let _ = input.focus();
                    input.select();
                }
                set_copy_state.set(Some("Select the link and copy it"));
            }
        });
    };
    let revoke = move |link: i64| {
        set_error.set(None);
        spawn_local(async move {
            match revoke_link(id, link).await {
                Ok(()) => refresh(),
                Err(err) => fail(err, "revoke the link"),
            }
        });
    };

    let describe = move |link: &ShareLink| {
        let by = if link.created_by_user_id == me.user_id {
            "you".to_string()
        } else {
            link.created_by_name.clone()
        };
        let used = match (link.uses, link.max_uses) {
            (0, _) => "not used yet".to_string(),
            (used, max) if used >= max => "used up".to_string(),
            (used, max) => format!("used {used} of {max} times"),
        };
        format!(
            "Made by {by} · {used} · expires {}",
            time_until(link.expires_at)
        )
    };

    view! {
        <form class="share-new" on:submit=create>
            <label>
                "Can be used by"
                <select on:change=move |ev| {
                    set_uses.set(event_target_value(&ev).parse().unwrap_or(1))
                }>
                    <option value="1" selected>"1 person"</option>
                    <option value="5">"5 people"</option>
                    <option value="25">"25 people"</option>
                </select>
            </label>
            <label>
                "Expires after"
                <select on:change=move |ev| {
                    set_days.set(event_target_value(&ev).parse().unwrap_or(7))
                }>
                    <option value="1">"1 day"</option>
                    <option value="7" selected>"7 days"</option>
                    <option value="30">"30 days"</option>
                </select>
            </label>
            <button type="submit" disabled=busy>"Create link"</button>
        </form>
        {move || created.get().map(|url| view! {
            <div class="share-created">
                <input
                    type="text"
                    readonly
                    aria-label="Share link"
                    node_ref=link_input
                    prop:value=url
                    on:focus=move |_| {
                        if let Some(input) = link_input.get_untracked() {
                            input.select();
                        }
                    }
                />
                <button type="button" on:click=copy>"Copy"</button>
            </div>
            <p class="hint" role="status">
                {move || copy_state.get().unwrap_or(
                    "Anyone with this link can join the list. It is shown only once.",
                )}
            </p>
        })}
        {move || error.get().map(|e| view! { <p class="status error" role="alert">{e}</p> })}
        {move || match links.get() {
            None => view! { <p class="hint">"Loading links…"</p> }.into_any(),
            Some(links) if links.is_empty() => view! {
                <p class="hint">"No active links."</p>
            }
            .into_any(),
            Some(links) => view! {
                <ul class="share-links" aria-label="Active links">
                    {links
                        .into_iter()
                        .map(|link| {
                            let link_id = link.id;
                            view! {
                                <li>
                                    <span>{describe(&link)}</span>
                                    <button class="secondary" on:click=move |_| revoke(link_id)>
                                        "Revoke"
                                    </button>
                                </li>
                            }
                        })
                        .collect_view()}
                </ul>
            }
            .into_any(),
        }}
    }
}

/// "in 3 days", "in 5 hours", "in 10 minutes".
fn time_until(unix_secs: i64) -> String {
    let secs = unix_secs - (js_sys::Date::now() / 1000.0) as i64;
    let (n, unit) = if secs >= 2 * 24 * 60 * 60 {
        (secs / (24 * 60 * 60), "days")
    } else if secs >= 2 * 60 * 60 {
        (secs / (60 * 60), "hours")
    } else {
        ((secs / 60).max(1), "minutes")
    };
    if n == 1 {
        format!("in 1 {}", unit.trim_end_matches('s'))
    } else {
        format!("in {n} {unit}")
    }
}
