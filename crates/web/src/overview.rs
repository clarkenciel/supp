use leptos::{ev::SubmitEvent, prelude::*};
use leptos_router::{components::A, hooks::use_navigate};
use supp_shared::DocId;

use crate::{repo::Repo, session::PageHeader};

#[component]
pub fn Overview() -> impl IntoView {
    let repo = expect_context::<Repo>();
    let navigate = use_navigate();
    let (title, set_title) = signal(String::new());

    let create = move |ev: SubmitEvent| {
        ev.prevent_default();
        let id = repo.create_list(title.get_untracked().trim().to_string());
        set_title.set(String::new());
        navigate(&format!("/lists/{id}"), Default::default());
    };

    view! {
        <PageHeader>
            <h1>"Lists"</h1>
        </PageHeader>
        <ul class="lists">
            <For each=move || repo.lists().get() key=|id| *id let(id)>
                <ListLink id />
            </For>
        </ul>
        <form class="add" on:submit=create>
            <input
                type="text"
                placeholder="New list"
                aria-label="New list title"
                prop:value=title
                on:input=move |ev| set_title.set(event_target_value(&ev))
            />
            <button type="submit">"Create"</button>
        </form>
    }
}

#[component]
fn ListLink(id: DocId) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let list = repo.list(id);
    let title = move || {
        list.map(|l| l.with(|l| l.title.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "Untitled".into())
    };
    let progress = move || {
        list.map(|l| {
            l.with(|l| {
                let done = l.items.values().filter(|i| i.done).count();
                format!("{done}/{}", l.items.len())
            })
        })
    };
    let done_share = move || {
        list.map(|l| {
            l.with(|l| {
                let done = l.items.values().filter(|i| i.done).count();
                let total = l.items.len().max(1);
                format!("width: {}%", done * 100 / total)
            })
        })
    };
    view! {
        <li>
            <A href=format!("/lists/{id}")>
                <span class="title">{title}</span>
                <span class="progress">{progress}</span>
                <span class="bar" aria-hidden="true">
                    <span style=done_share></span>
                </span>
            </A>
        </li>
    }
}
