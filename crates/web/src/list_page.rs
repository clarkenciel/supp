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
use web_sys::{Element, HtmlElement, ScrollIntoViewOptions, ScrollLogicalPosition};

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
    let on_scroll = window_event_listener(ev::scroll, move |_| mover.measure());
    let on_resize = window_event_listener(ev::resize, move |_| mover.measure());
    let on_key = window_event_listener(ev::keydown, move |ev| {
        if mover.moving.get_untracked().is_none() || typing(&ev) {
            return;
        }
        match ev.key().as_str() {
            "Escape" => mover.finish(),
            "ArrowUp" => mover.step(-1),
            "ArrowDown" => mover.step(1),
            _ => return,
        }
        ev.prevent_default();
    });
    on_cleanup(move || {
        on_scroll.remove();
        on_resize.remove();
        on_key.remove();
    });
    // The list can change under a move, here or on another peer: re-read
    // the gap once the rows have re-rendered, and give up if the item went.
    Effect::new(move |_| {
        let Some(moving) = mover.moving.get() else {
            return;
        };
        if list.with(|l| !l.items.contains_key(&moving)) {
            mover.moving.set(None);
            return;
        }
        request_animation_frame(move || mover.measure());
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
            <Show when=move || mover.zone_before(None)>
                <DropZone mover />
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
                <p class="moving-item">{moving_text}</p>
                <button class="danger" on:click=move |_| mover.finish()>
                    "Cancel"
                </button>
            </div>
        </Show>
    }
}

/// Pick-and-place reordering. An item's grip picks it up and a drop zone
/// opens in the list at the middle of the screen; scrolling moves the zone
/// from gap to gap, and tapping it drops the item there. Moving an item any
/// distance is one pick and one drop, and nothing is dragged while scrolling.
#[derive(Clone, Copy)]
struct Mover {
    repo: Repo,
    doc: DocId,
    list: Signal<ListDoc>,
    /// The item picked up.
    moving: RwSignal<Option<String>>,
    /// The item it would land before, or `None` for the end. Read from the
    /// rows on screen, so it follows scrolling and other peers' edits.
    before: RwSignal<Option<String>>,
}

impl Mover {
    fn new(repo: Repo, doc: DocId, list: Signal<ListDoc>) -> Self {
        Self {
            repo,
            doc,
            list,
            moving: RwSignal::new(None),
            before: RwSignal::new(None),
        }
    }

    /// Whether the drop zone goes before the row `item_id` (`None`: the end).
    fn zone_before(self, item_id: Option<&str>) -> bool {
        self.moving.with(Option::is_some) && self.before.with(|b| b.as_deref() == item_id)
    }

    /// Where the drop zone would put the item, in words.
    fn hint(self) -> Option<String> {
        let moving = self.moving.get()?;
        let before = self.before.get();
        self.list.with(|l| {
            let slot = slot(l, &moving, before.as_deref())?;
            let others: Vec<_> = l
                .ordered()
                .into_iter()
                .filter(|(id, _)| *id != moving)
                .collect();
            Some(if slot.current {
                "Where it is now".to_string()
            } else if slot.index == 0 {
                "At the top".to_string()
            } else if slot.index == others.len() {
                "At the end".to_string()
            } else {
                let (a, b) = (&others[slot.index - 1].1.text, &others[slot.index].1.text);
                format!("Between “{a}” and “{b}”")
            })
        })
    }

    fn start(self, item_id: String) {
        self.moving.set(Some(item_id.clone()));
        // Once the list has its extra room, put the item at the middle of
        // the screen, so the first gap offered is its own.
        request_animation_frame(move || {
            if let Some(row) = row(&item_id) {
                let options = ScrollIntoViewOptions::new();
                options.set_block(ScrollLogicalPosition::Center);
                row.scroll_into_view_with_scroll_into_view_options(&options);
            }
            self.measure();
            request_animation_frame(focus_zone);
        });
    }

    fn finish(self) {
        let Some(item_id) = self.moving.get_untracked() else {
            return;
        };
        self.moving.set(None);
        request_animation_frame(move || {
            let Some(row) = row(&item_id) else {
                return;
            };
            let options = ScrollIntoViewOptions::new();
            options.set_block(ScrollLogicalPosition::Nearest);
            row.scroll_into_view_with_scroll_into_view_options(&options);
            if let Ok(Some(grip)) = row.query_selector(".grip") {
                let _ = grip.unchecked_into::<HtmlElement>().focus();
            }
        });
    }

    fn drop(self) {
        self.measure();
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        let before = self.before.get_untracked();
        self.repo.change(self.doc, |l| {
            let Some(slot) = slot(l, &moving, before.as_deref()).filter(|s| !s.current) else {
                return;
            };
            let position = l.position_at(slot.index, Some(&moving));
            if let Some(item) = l.items.get_mut(&moving) {
                item.position = position;
            }
        });
        self.finish();
    }

    /// Finds the gap at the middle of the screen: before the first row,
    /// other than the moving one, whose middle is below it.
    fn measure(self) {
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        let (rows, line) = layout(&moving);
        let before = rows
            .into_iter()
            .find(|(_, top, bottom)| (top + bottom) / 2.0 > line)
            .map(|(id, ..)| id);
        if self.before.get_untracked() != before {
            self.before.set(before);
        }
    }

    /// Scrolls the gap `delta` gaps away to the middle, for the keyboard.
    fn step(self, delta: isize) {
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        let before = self.before.get_untracked();
        let (rows, line) = layout(&moving);
        let current = before
            .and_then(|b| rows.iter().position(|(id, ..)| *id == b))
            .unwrap_or(rows.len());
        let Some(target) = current
            .checked_add_signed(delta)
            .filter(|t| *t <= rows.len())
        else {
            return;
        };
        let gap = match (rows.get(target), rows.last()) {
            (Some((_, top, _)), _) => *top,
            (None, Some((_, _, bottom))) => *bottom,
            (None, None) => return,
        };
        window().scroll_by_with_x_and_y(0.0, gap - line);
        self.measure();
        // The zone has moved to another row, taking the focus with it.
        request_animation_frame(focus_zone);
    }
}

#[component]
fn DropZone(mover: Mover) -> impl IntoView {
    view! {
        <li class="drop-zone">
            <button on:click=move |_| mover.drop()>
                "Drop here"
                <span class="hint">{move || mover.hint()}</span>
            </button>
        </li>
    }
}

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

/// The rows other than the moving one, top to bottom, with their tops and
/// bottoms on screen as if the drop zone weren't in the list; and the line
/// a gap must be at for the zone, once in it, to sit at the middle of the
/// screen. Leaving the zone out keeps it from pushing the rows it's
/// measured against, which would flip it between two gaps.
fn layout(moving: &str) -> (Vec<(String, f64, f64)>, f64) {
    let zone = document()
        .query_selector("ul.items > .drop-zone")
        .ok()
        .flatten()
        .map(|z| z.get_bounding_client_rect());
    let (zone_top, zone_height) = zone.map_or((f64::INFINITY, 0.0), |z| (z.top(), z.height()));
    let rows = rows()
        .into_iter()
        .filter(|(id, _)| id != moving)
        .map(|(id, row)| {
            let rect = row.get_bounding_client_rect();
            let shift = if rect.top() >= zone_top {
                zone_height
            } else {
                0.0
            };
            (id, rect.top() - shift, rect.bottom() - shift)
        })
        .collect();
    let height = window().inner_height().ok().and_then(|h| h.as_f64());
    (rows, (height.unwrap_or(0.0) - zone_height) / 2.0)
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

fn focus_zone() {
    if let Ok(Some(button)) = document().query_selector(".drop-zone button") {
        let _ = button.unchecked_into::<HtmlElement>().focus();
    }
}

/// Whether a key is going to a text field rather than the page.
fn typing(ev: &web_sys::KeyboardEvent) -> bool {
    ev.target()
        .and_then(|t| t.dyn_into::<web_sys::HtmlInputElement>().ok())
        .is_some_and(|input| input.type_() == "text")
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
    let zone_here = {
        let item_id = item_id.clone();
        move || mover.zone_before(Some(&item_id))
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
    view! {
        <Show when=zone_here>
            <DropZone mover />
        </Show>
        <li
            data-id=item_id
            class:done=move || item.with(|i| i.as_ref().is_some_and(|i| i.done))
            class:moving=is_moving
        >
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
            <button class="icon" aria-label="Delete item" on:click=remove>
                "×"
            </button>
        </li>
    }
}
