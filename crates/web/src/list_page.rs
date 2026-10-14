use leptos::{ev::SubmitEvent, prelude::*};
use leptos_router::{
    components::A,
    hooks::{use_navigate, use_params_map},
};
use supp_shared::{
    DocId,
    schema::{Item, ListDoc},
};

use crate::repo::Repo;

#[component]
pub fn ListPage() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let params = use_params_map();
    let found = move || {
        let id: DocId = params.with(|p| p.get("id")?.parse().ok())?;
        Some((id, repo.list(id)?))
    };
    move || {
        match found() {
        Some((id, list)) => view! { <ListView id list /> }.into_any(),
        None => view! {
            <p class="status">"This list doesn't exist on this device. " <A href="/">"All lists"</A></p>
        }
        .into_any(),
    }
    }
}

#[component]
fn ListView(id: DocId, list: Signal<ListDoc>) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let navigate = use_navigate();
    let (draft, set_draft) = signal(String::new());

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
        </header>
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
