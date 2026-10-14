use leptos::{ev::SubmitEvent, prelude::*};
use leptos_router::{
    components::A,
    hooks::{use_navigate, use_params_map},
};
use supp_shared::{
    DocId,
    schema::{Item, ListDoc},
};

use crate::{
    repo::Repo,
    session::{Session, SessionChecked},
    share::SharePanel,
    sync::SyncStatus,
};

#[component]
pub fn ListPage() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let session = expect_context::<RwSignal<Session>>();
    let status = expect_context::<RwSignal<SyncStatus>>();
    let checked = expect_context::<SessionChecked>().0;
    let params = use_params_map();
    let id = Memo::new(move |_| params.with(|p| p.get("id")?.parse::<DocId>().ok()));
    // On this device, though perhaps still empty: a list just joined
    // through a share link arrives from the sync loop's Welcome, and its
    // contents a moment later. Memos, so other lists coming and going don't
    // re-render the page.
    let local = Memo::new(move |_| id.get().filter(|id| repo.lists().with(|l| l.contains(id))));
    let ready = Memo::new(move |_| {
        let id = local.get()?;
        let list = repo.list(id)?;
        list.track();
        repo.has_contents(id).then_some(id)
    });
    let loading = move || {
        let signed_in = matches!(session.get(), Session::SignedIn(_));
        !checked.get()
            || signed_in
                && match status.get() {
                    SyncStatus::Idle | SyncStatus::Connecting => true,
                    // The server has listed the account's lists by now.
                    SyncStatus::Online => local.get().is_some(),
                    SyncStatus::Offline | SyncStatus::UpgradeRequired => false,
                }
    };
    move || match ready.get() {
        Some(id) => {
            let list = repo.list(id).expect("ready lists are local");
            view! { <ListView id list /> }.into_any()
        }
        None if loading() => view! { <p class="status" role="status">"Loading…"</p> }.into_any(),
        None if local.get().is_some() => view! {
            <p class="status">
                "This list hasn't downloaded to this device yet. "
                "It will once you're online and signed in. " <A href="/">"All lists"</A>
            </p>
        }
        .into_any(),
        None => view! {
            <p class="status">
                "This list doesn't exist on this device. " <A href="/">"All lists"</A>
            </p>
        }
        .into_any(),
    }
}

#[component]
fn ListView(id: DocId, list: Signal<ListDoc>) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let navigate = use_navigate();
    let (draft, set_draft) = signal(String::new());
    let (sharing, set_sharing) = signal(false);

    let add = move |ev: SubmitEvent| {
        ev.prevent_default();
        let text = draft.get_untracked().trim().to_string();
        if text.is_empty() {
            return;
        }
        repo.change(id, |l| {
            let position = l.position_at(l.items.len(), None);
            let item = Item {
                text,
                done: false,
                position,
            };
            l.items.insert(uuid::Uuid::new_v4().to_string(), item);
        });
        set_draft.set(String::new());
    };

    let delete_list = move |_| {
        let confirmed = window()
            .confirm_with_message("Delete this list from this device?")
            .unwrap_or(false);
        if confirmed {
            repo.delete_list(id);
            navigate("/", Default::default());
        }
    };

    let ordered_ids = move || {
        list.with(|l| {
            l.ordered()
                .into_iter()
                .map(|(id, _)| id.to_string())
                .collect::<Vec<_>>()
        })
    };
    let any_done = move || list.with(|l| l.items.values().any(|i| i.done));

    view! {
        <header>
            <A href="/" attr:class="back" attr:aria-label="All lists">"‹"</A>
            <input
                class="list-title"
                type="text"
                placeholder="Untitled"
                aria-label="List title"
                prop:value=move || list.with(|l| l.title.clone())
                on:change=move |ev| {
                    let title = event_target_value(&ev);
                    repo.change(id, |l| l.title = title);
                }
            />
            <button
                class="secondary"
                aria-expanded=move || sharing.get().to_string()
                on:click=move |_| set_sharing.update(|s| *s = !*s)
            >
                "Share"
            </button>
        </header>
        <Show when=move || sharing.get()>
            <SharePanel id />
        </Show>
        <ul class="items">
            <For each=ordered_ids key=|item_id| item_id.clone() let(item_id)>
                <ItemRow doc=id list item_id />
            </For>
        </ul>
        <form class="add" on:submit=add>
            <input
                type="text"
                placeholder="Add item"
                aria-label="New item"
                prop:value=draft
                on:input=move |ev| set_draft.set(event_target_value(&ev))
            />
            <button type="submit">"Add"</button>
        </form>
        <footer>
            <Show when=any_done>
                <button
                    class="secondary"
                    on:click=move |_| repo.change(id, |l| l.items.retain(|_, i| !i.done))
                >
                    "Clear done"
                </button>
            </Show>
            <button class="danger" on:click=delete_list>
                "Delete list"
            </button>
        </footer>
    }
}

#[component]
fn ItemRow(doc: DocId, list: Signal<ListDoc>, item_id: String) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let item = Memo::new({
        let item_id = item_id.clone();
        move |_| list.with(|l| l.items.get(&item_id).cloned())
    });
    let index = Memo::new({
        let item_id = item_id.clone();
        move |_| list.with(|l| l.ordered().iter().position(|(id, _)| *id == item_id))
    });
    let len = move || list.with(|l| l.items.len());

    let edit = {
        let item_id = item_id.clone();
        move |f: Box<dyn FnOnce(&mut Item)>| {
            repo.change(doc, |l| {
                if let Some(item) = l.items.get_mut(&item_id) {
                    f(item);
                }
            })
        }
    };
    let shift = {
        let item_id = item_id.clone();
        move |delta: isize| {
            let Some(i) = index.get_untracked() else {
                return;
            };
            let Some(target) = i.checked_add_signed(delta) else {
                return;
            };
            repo.change(doc, |l| {
                let position = l.position_at(target, Some(&item_id));
                if let Some(item) = l.items.get_mut(&item_id) {
                    item.position = position;
                }
            });
        }
    };
    let remove = {
        let item_id = item_id.clone();
        move |_| {
            repo.change(doc, |l| {
                l.items.remove(&item_id);
            })
        }
    };

    let (edit_done, edit_text) = (edit.clone(), edit);
    let (shift_up, shift_down) = (shift.clone(), shift);
    view! {
        <li class:done=move || item.with(|i| i.as_ref().is_some_and(|i| i.done))>
            <input
                type="checkbox"
                aria-label="Done"
                prop:checked=move || item.with(|i| i.as_ref().is_some_and(|i| i.done))
                on:change=move |ev| {
                    let done = event_target_checked(&ev);
                    edit_done(Box::new(move |i| i.done = done));
                }
            />
            <input
                class="text"
                type="text"
                aria-label="Item"
                prop:value=move || item.with(|i| i.as_ref().map(|i| i.text.clone()).unwrap_or_default())
                on:change=move |ev| {
                    let text = event_target_value(&ev);
                    edit_text(Box::new(move |i| i.text = text));
                }
            />
            <button
                class="icon"
                aria-label="Move up"
                disabled=move || index.get() == Some(0)
                on:click=move |_| shift_up(-1)
            >
                "↑"
            </button>
            <button
                class="icon"
                aria-label="Move down"
                disabled=move || index.get().is_none_or(|i| i + 1 >= len())
                on:click=move |_| shift_down(1)
            >
                "↓"
            </button>
            <button class="icon" aria-label="Delete item" on:click=remove>
                "×"
            </button>
        </li>
    }
}
