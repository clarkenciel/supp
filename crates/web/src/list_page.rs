use leptos::{
    ev::{self, SubmitEvent},
    prelude::*,
};
use leptos_router::{
    components::A,
    hooks::{use_navigate, use_params_map},
};
use supp_shared::{
    DocId,
    schema::{Item, ListDoc},
};
use wasm_bindgen::JsCast;
use web_sys::{Element, FocusOptions, HtmlElement};

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

    let mover = Mover::new(repo, id, list);
    let on_key = window_event_listener(ev::keydown, move |ev| {
        if ev.key() == "Escape" && mover.moving.get_untracked().is_some() {
            mover.finish();
        }
    });
    on_cleanup(move || on_key.remove());
    // The item can go under a move, here or on another peer.
    Effect::new(move |_| {
        let Some(moving) = mover.moving.get() else {
            return;
        };
        if list.with(|l| !l.items.contains_key(&moving)) {
            mover.moving.set(None);
        }
    });
    let moving_text = move || {
        let moving = mover.moving.get()?;
        list.with(|l| l.items.get(&moving).map(|i| i.text.clone()))
    };

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
        <ul class="items" class:moving=move || mover.moving.get().is_some()>
            <For each=ordered_ids key=|item_id| item_id.clone() let(item_id)>
                <ItemRow doc=id list mover item_id />
            </For>
            <Show when=move || mover.target(None)>
                <li class="end-gutter">
                    <Gutter mover before=None />
                </li>
            </Show>
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
        <Show when=move || mover.moving.get().is_some()>
            <div class="mover" role="region" aria-label="Moving item">
                <div class="mover-text">
                    <p class="moving-item">{moving_text}</p>
                    <p class="hint">"Tap an arrow to move it there"</p>
                </div>
                <button class="pop" on:click=move |_| mover.finish()>
                    "Cancel"
                </button>
            </div>
        </Show>
    }
}

/// Pick-and-place reordering. An item's grip picks it up, and every gap
/// between rows it could move to becomes a target: a line with a flag,
/// over the gap without making it any taller. Scrolling is the page's own;
/// tapping a gap drops the item there.
#[derive(Clone, Copy)]
struct Mover {
    repo: Repo,
    doc: DocId,
    list: Signal<ListDoc>,
    /// The item picked up.
    moving: RwSignal<Option<String>>,
}

impl Mover {
    fn new(repo: Repo, doc: DocId, list: Signal<ListDoc>) -> Self {
        Self {
            repo,
            doc,
            list,
            moving: RwSignal::new(None),
        }
    }

    /// Whether the gap before the row `before` (`None`: the end) is a
    /// target: something is moving, and that's not where it already is.
    fn target(self, before: Option<&str>) -> bool {
        let Some(moving) = self.moving.get() else {
            return false;
        };
        self.list
            .with(|l| slot(l, &moving, before).is_some_and(|s| !s.current))
    }

    /// What dropping in the gap before `before` does, in words.
    fn describe(self, before: Option<&str>) -> String {
        let Some(moving) = self.moving.get() else {
            return String::new();
        };
        self.list.with(|l| {
            let others: Vec<_> = l
                .ordered()
                .into_iter()
                .filter(|(id, _)| *id != moving)
                .collect();
            match slot(l, &moving, before).map(|s| s.index) {
                Some(0) => "Move to the top".to_string(),
                Some(i) if i == others.len() => "Move to the end".to_string(),
                Some(i) => {
                    let (a, b) = (&others[i - 1].1.text, &others[i].1.text);
                    format!("Move between “{a}” and “{b}”")
                }
                None => String::new(),
            }
        })
    }

    fn start(self, item_id: String) {
        self.moving.set(Some(item_id.clone()));
        // For the keyboard: the target just below the item, else the
        // nearest above.
        request_animation_frame(move || {
            let rows = rows();
            let Some(i) = rows.iter().position(|(id, _)| *id == item_id) else {
                return;
            };
            let below = rows.get(i + 2).map(|(_, row)| row.clone());
            let target = below
                .and_then(|row| row.query_selector(".gutter").ok().flatten())
                .or_else(|| {
                    document()
                        .query_selector(".end-gutter .gutter")
                        .ok()
                        .flatten()
                })
                .or_else(|| {
                    let above = rows.get(i.checked_sub(1)?)?;
                    above.1.query_selector(".gutter").ok().flatten()
                });
            if let Some(target) = target {
                focus(&target);
            }
        });
    }

    fn finish(self) {
        let Some(item_id) = self.moving.get_untracked() else {
            return;
        };
        self.moving.set(None);
        request_animation_frame(move || focus_grip(&item_id));
    }

    /// Drops the item in the gap before `before` (`None`: the end), which
    /// is at `gap` down the screen; the item appears there, with the rows
    /// either side of the gap staying where they are.
    fn drop(self, before: Option<String>, gap: f64) {
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        self.repo.change(self.doc, |l| {
            let Some(slot) = slot(l, &moving, before.as_deref()).filter(|s| !s.current) else {
                return;
            };
            let position = l.position_at(slot.index, Some(&moving));
            if let Some(item) = l.items.get_mut(&moving) {
                item.position = position;
            }
        });
        self.moving.set(None);
        request_animation_frame(move || {
            keep(&moving, gap);
            focus_grip(&moving);
        });
    }
}

/// The target for the gap before the row `before` (`None`: the end).
#[component]
fn Gutter(mover: Mover, before: Option<String>) -> impl IntoView {
    let label = {
        let before = before.clone();
        move || mover.describe(before.as_deref())
    };
    let drop = move |ev: ev::MouseEvent| {
        let Some(gutter) = ev.current_target() else {
            return;
        };
        let rect = gutter
            .unchecked_into::<Element>()
            .get_bounding_client_rect();
        mover.drop(before.clone(), rect.top() + rect.height() / 2.0);
    };
    view! {
        <button class="gutter" aria-label=label on:click=drop>
            <svg class="arrow" viewBox="0 0 40 28" aria-hidden="true">
                <path class="shadow" transform="translate(3 3)" d=ARROW />
                <path class="face" d=ARROW />
            </svg>
        </button>
    }
}

/// A chunky arrow pointing left into its gap.
const ARROW: &str = "M3 13L15 4Q17 3 17 5L17 9L31 9Q34 9 34 12L34 14Q34 17 31 17L17 17L17 21Q17 23 15 22Z";

/// Where an item would land in front of `before` (or at the end).
struct Slot {
    /// Index among the other items, for [`ListDoc::position_at`].
    index: usize,
    /// Whether that's where it already is.
    current: bool,
}

fn slot(list: &ListDoc, moving: &str, before: Option<&str>) -> Option<Slot> {
    let ordered = list.ordered();
    let others: Vec<_> = ordered.iter().filter(|(id, _)| *id != moving).collect();
    let index = match before {
        Some(before) => others.iter().position(|(id, _)| *id == before)?,
        None => others.len(),
    };
    let current = ordered.iter().position(|(id, _)| *id == moving)? == index;
    Some(Slot { index, current })
}

/// The item rows on screen, top to bottom, by item ID.
fn rows() -> Vec<(String, Element)> {
    let Ok(nodes) = document().query_selector_all("ul.items > li[data-id]") else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|i| nodes.item(i)?.dyn_into::<Element>().ok())
        .filter_map(|row| Some((row.get_attribute("data-id")?, row)))
        .collect()
}

fn row(item_id: &str) -> Option<Element> {
    rows()
        .into_iter()
        .find(|(id, _)| id == item_id)
        .map(|(_, row)| row)
}

/// Scrolls so the row for `item_id` has its top at `anchor` on screen.
fn keep(item_id: &str, anchor: f64) {
    if let Some(row) = row(item_id) {
        let top = row.get_bounding_client_rect().top();
        window().scroll_by_with_x_and_y(0.0, top - anchor);
    }
}

fn focus(el: &Element) {
    let options = FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = el
        .unchecked_ref::<HtmlElement>()
        .focus_with_options(&options);
}

fn focus_grip(item_id: &str) {
    if let Some(Ok(Some(grip))) = row(item_id).map(|r| r.query_selector(".grip")) {
        focus(&grip);
    }
}

#[component]
fn ItemRow(doc: DocId, list: Signal<ListDoc>, mover: Mover, item_id: String) -> impl IntoView {
    let repo = expect_context::<Repo>();
    let item = Memo::new({
        let item_id = item_id.clone();
        move |_| list.with(|l| l.items.get(&item_id).cloned())
    });
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
    let is_moving = Memo::new({
        let item_id = item_id.clone();
        move |_| mover.moving.with(|m| m.as_ref() == Some(&item_id))
    });
    let gutter_here = {
        let item_id = item_id.clone();
        move || mover.target(Some(&item_id))
    };
    let pick = {
        let item_id = item_id.clone();
        move |_| {
            if is_moving.get_untracked() {
                mover.finish();
            } else {
                mover.start(item_id.clone());
            }
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
    let before = Some(item_id.clone());
    view! {
        <li
            data-id=item_id
            class:done=move || item.with(|i| i.as_ref().is_some_and(|i| i.done))
            class:moving=is_moving
        >
            <Show when=gutter_here.clone()>
                <Gutter mover before=before.clone() />
            </Show>
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
                class="icon grip"
                aria-label="Move"
                aria-pressed=move || is_moving.get().to_string()
                on:click=pick
            >
                "⠿"
            </button>
            <button class="icon delete" aria-label="Delete item" on:click=remove>
                "×"
            </button>
        </li>
    }
}
