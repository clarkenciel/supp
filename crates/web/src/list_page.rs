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
use web_sys::{Element, HtmlElement};

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
        if mover.moving.get_untracked().is_none() || typing(&ev) {
            return;
        }
        match ev.key().as_str() {
            "Escape" => mover.finish(),
            "ArrowUp" => _ = mover.step(-1, EDGE),
            "ArrowDown" => _ = mover.step(1, EDGE),
            _ => return,
        }
        ev.prevent_default();
    });
    // While moving, swipes and the wheel move the zone rather than scroll
    // the page (see `lock_page`).
    let on_down = window_event_listener(ev::pointerdown, move |ev| {
        let in_bar = ev
            .target()
            .and_then(|t| t.dyn_into::<Element>().ok())
            .and_then(|el| el.closest(".mover").ok().flatten())
            .is_some();
        if !in_bar {
            mover.pointer_down(f64::from(ev.client_y()), ev.time_stamp());
        }
    });
    let on_move = window_event_listener(ev::pointermove, move |ev| {
        mover.pointer_move(f64::from(ev.client_y()), ev.time_stamp());
    });
    let on_up = window_event_listener(ev::pointerup, move |ev| mover.pointer_up(ev.time_stamp()));
    let on_cancel = window_event_listener(ev::pointercancel, move |ev| {
        mover.pointer_up(ev.time_stamp())
    });
    let on_wheel = window_event_listener(ev::wheel, move |ev| {
        let unit = match ev.delta_mode() {
            web_sys::WheelEvent::DOM_DELTA_LINE => 16.0,
            web_sys::WheelEvent::DOM_DELTA_PAGE => viewport_height(),
            _ => 1.0,
        };
        mover.nudge(ev.delta_y() * unit);
    });
    on_cleanup(move || {
        on_key.remove();
        on_down.remove();
        on_move.remove();
        on_up.remove();
        on_cancel.remove();
        on_wheel.remove();
        lock_page(false);
    });
    // The list can change under a move, here or on another peer: give up
    // if the item went, and go back to its own gap if the zone's row went.
    Effect::new(move |_| {
        let Some(moving) = mover.moving.get() else {
            return;
        };
        list.with(|l| {
            if !l.items.contains_key(&moving) {
                mover.end(moving, None);
                return;
            }
            let lost = mover
                .before
                .with_untracked(|b| b.as_ref().is_some_and(|b| !l.items.contains_key(b)));
            if lost {
                let ordered = l.ordered();
                let own = ordered.iter().position(|(id, _)| *id == moving);
                let next = own
                    .and_then(|i| ordered.get(i + 1))
                    .map(|(id, _)| id.to_string());
                mover.before.set(next);
            }
        });
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
/// opens in its gap, with nothing else on screen moving. Swiping (or the
/// wheel) moves the zone a gap per [`GAP_DISTANCE`], with the page
/// following to keep it in view, as do the arrow keys a gap at a time and
/// tapping a row (above it on its top half, below on its bottom half);
/// tapping the zone drops the item there. How far a swipe moves the zone
/// doesn't depend on how far the page can scroll, so it feels the same in
/// short lists and long, and moving an item any distance is one pick and
/// one drop.
#[derive(Clone, Copy)]
struct Mover {
    repo: Repo,
    doc: DocId,
    list: Signal<ListDoc>,
    /// The item picked up.
    moving: RwSignal<Option<String>>,
    /// The item it would land before, or `None` for the end.
    before: RwSignal<Option<String>>,
    /// Room added below the rows, so the zone can clear the bar at the end.
    room: RwSignal<f64>,
    gesture: StoredValue<Gesture>,
}

#[derive(Clone, Copy, Default)]
struct Gesture {
    /// Where (down the screen) and when the pointer was last, while down.
    last: Option<(f64, f64)>,
    /// In pixels per millisecond, for a fling.
    velocity: f64,
    /// How far the pointer has gone since it went down, to tell a swipe
    /// from a tap.
    travel: f64,
    /// Swiped distance not yet turned into whole gaps.
    carry: f64,
    /// Bumped to stop a fling.
    fling: u32,
}

/// Swipe distance, in CSS pixels, that moves the zone one gap.
const GAP_DISTANCE: f64 = 64.0;

/// Pointer travel beyond which a press is a swipe and not a tap.
const TAP_SLOP: f64 = 10.0;

/// How far, as a share of the screen, the page keeps the zone from its top
/// and bottom edges as swipes and keys move it.
const EDGE: f64 = 0.2;

impl Mover {
    fn new(repo: Repo, doc: DocId, list: Signal<ListDoc>) -> Self {
        Self {
            repo,
            doc,
            list,
            moving: RwSignal::new(None),
            before: RwSignal::new(None),
            room: RwSignal::new(0.0),
            gesture: StoredValue::new(Gesture::default()),
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
        if self.moving.get_untracked().is_some() {
            self.clear_room();
        }
        let Some(picked) = row(&item_id) else {
            return;
        };
        let rect = picked.get_bounding_client_rect();
        let next = rows()
            .into_iter()
            .filter(|(id, _)| *id != item_id)
            .find(|(_, row)| row.get_bounding_client_rect().top() >= rect.bottom())
            .map(|(id, _)| id);
        lock_page(true);
        self.gesture.update_value(|g| *g = Gesture::default());
        self.moving.set(Some(item_id.clone()));
        self.before.set(next);
        // Once the zone is in, and before the next paint.
        request_animation_frame(move || {
            keep(&item_id, rect.top());
            self.settle(0.0);
            focus_zone();
        });
    }

    /// Moves the zone to the gap `before` (`None`: the end), keeping it
    /// `edge` (a share of the screen) from the screen's edges.
    fn place(self, before: Option<String>, edge: f64) {
        self.before.set(before);
        self.settle(edge);
        // The zone is a new element in its new place.
        request_animation_frame(focus_zone);
    }

    /// Moves the zone to the gap nearest a tap at `y` on a row: above it
    /// for its top half, below for its bottom half. A tap on the moving
    /// item's own row puts the zone back where it started.
    fn tap(self, item_id: &str, y: f64) {
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        if self.swiped() {
            return;
        }
        let rows = rows();
        let Some(i) = rows.iter().position(|(id, _)| id == item_id) else {
            return;
        };
        let rect = rows[i].1.get_bounding_client_rect();
        let below = item_id == moving || y > rect.top() + rect.height() / 2.0;
        let before = rows[i + usize::from(below)..]
            .iter()
            .map(|(id, _)| id)
            .find(|id| **id != moving)
            .cloned();
        self.place(before, 0.0);
    }

    /// Moves the zone `delta` gaps, if it can go that far.
    fn step(self, delta: isize, edge: f64) -> bool {
        let Some(moving) = self.moving.get_untracked() else {
            return false;
        };
        let before = self.before.get_untracked();
        let rows = layout(moving.as_str());
        let current = before
            .and_then(|b| rows.iter().position(|(id, ..)| *id == b))
            .unwrap_or(rows.len());
        let Some(target) = current
            .checked_add_signed(delta)
            .filter(|t| *t <= rows.len())
        else {
            return false;
        };
        self.place(rows.get(target).map(|(id, ..)| id.clone()), edge);
        true
    }

    /// Moves the zone by a swiped (or wheeled) distance: down the list for
    /// positive `dy`, a gap per [`GAP_DISTANCE`].
    fn nudge(self, dy: f64) {
        if self.moving.get_untracked().is_none() {
            return;
        }
        let mut carry = self.gesture.with_value(|g| g.carry) + dy;
        while carry.abs() >= GAP_DISTANCE {
            if !self.step(carry.signum() as isize, EDGE) {
                // At the first or last gap: don't bank swiping past it.
                carry = 0.0;
                break;
            }
            carry -= carry.signum() * GAP_DISTANCE;
        }
        self.gesture.update_value(|g| g.carry = carry);
    }

    fn pointer_down(self, y: f64, at: f64) {
        if self.moving.get_untracked().is_none() {
            return;
        }
        self.gesture.update_value(|g| {
            g.last = Some((y, at));
            g.velocity = 0.0;
            g.travel = 0.0;
            g.fling += 1;
        });
    }

    fn pointer_move(self, y: f64, at: f64) {
        let Some((last_y, last_at)) = self.gesture.with_value(|g| g.last) else {
            return;
        };
        // Finger up the screen is the list scrolling on: the zone goes down.
        let dy = last_y - y;
        let dt = (at - last_at).max(1.0);
        self.gesture.update_value(|g| {
            g.last = Some((y, at));
            g.velocity = 0.8 * dy / dt + 0.2 * g.velocity;
            g.travel += dy.abs();
        });
        self.nudge(dy);
    }

    fn pointer_up(self, at: f64) {
        let Some((_, last_at)) = self.gesture.with_value(|g| g.last) else {
            return;
        };
        let (velocity, fling) = self.gesture.with_value(|g| (g.velocity, g.fling));
        self.gesture.update_value(|g| g.last = None);
        // A finger that stopped before lifting doesn't fling.
        if velocity.abs() > 0.3 && at - last_at < 100.0 {
            self.fling(fling, velocity, js_sys::Date::now());
        }
    }

    /// Keeps the zone going after a quick swipe, slowing to a stop.
    fn fling(self, id: u32, velocity: f64, last: f64) {
        request_animation_frame(move || {
            if self.gesture.with_value(|g| g.fling) != id {
                return;
            }
            let now = js_sys::Date::now();
            let dt = (now - last).max(1.0);
            self.nudge(velocity * dt);
            let velocity = velocity * 0.95_f64.powf(dt / 16.0);
            if velocity.abs() > 0.05 {
                self.fling(id, velocity, now);
            }
        });
    }

    /// Whether the last press was a swipe, so its click isn't a tap.
    fn swiped(self) -> bool {
        self.gesture.with_value(|g| g.travel) > TAP_SLOP
    }

    /// With the zone going to the gap `before`: scrolls it into view clear
    /// of the bar, and `edge` from the screen's edges where the page can
    /// scroll that far. Room is added below the rows only so the zone can
    /// clear the bar at the end. Works from the rows, not the zone, so it
    /// needn't wait for the zone to move.
    fn settle(self, edge: f64) {
        let rows = layout(None);
        let gap = match self.before.get_untracked() {
            Some(before) => rows.iter().find(|(id, ..)| *id == before).map(|r| r.1),
            None => rows.last().map(|r| r.2),
        };
        let Some(gap) = gap else {
            return;
        };
        let zone = rect_of(".drop-zone").map_or(0.0, |r| r.height());
        let bar = rect_of(".mover").map_or(0.0, |r| r.height());
        let height = viewport_height();
        let bottom = height - bar;
        let need = (gap + zone - bottom).max(0.0);
        let short = scroll_y() + need - max_scroll();
        if short > 0.0 {
            self.room.update(|room| *room += short);
            set_room(Some(self.room.get_untracked()));
        }
        let margin = height * edge;
        let want = if gap < margin {
            gap - margin
        } else {
            (gap + zone - (bottom - margin)).max(0.0)
        };
        let shift = want.clamp(-scroll_y(), max_scroll() - scroll_y()).max(need);
        if shift != 0.0 {
            window().scroll_by_with_x_and_y(0.0, shift);
        }
    }

    fn clear_room(self) {
        self.room.set(0.0);
        set_room(None);
    }

    /// Ends the move. Whichever row ends up at `anchor`'s item is kept at
    /// that height on screen, as the room and zone go away.
    fn finish(self) {
        let Some(item_id) = self.moving.get_untracked() else {
            return;
        };
        let anchor = row(&item_id).map(|r| r.get_bounding_client_rect().top());
        self.end(item_id, anchor);
    }

    fn end(self, item_id: String, anchor: Option<f64>) {
        self.moving.set(None);
        self.gesture.update_value(|g| *g = Gesture::default());
        request_animation_frame(move || {
            self.clear_room();
            if let Some(anchor) = anchor {
                keep(&item_id, anchor);
            }
            lock_page(false);
            if let Some(Ok(Some(grip))) = row(&item_id).map(|r| r.query_selector(".grip")) {
                let _ = grip.unchecked_into::<HtmlElement>().focus();
            }
        });
    }

    fn drop(self) {
        if self.swiped() {
            return;
        }
        let Some(moving) = self.moving.get_untracked() else {
            return;
        };
        let before = self.before.get_untracked();
        let mut moved = false;
        self.repo.change(self.doc, |l| {
            let Some(slot) = slot(l, &moving, before.as_deref()).filter(|s| !s.current) else {
                return;
            };
            let position = l.position_at(slot.index, Some(&moving));
            if let Some(item) = l.items.get_mut(&moving) {
                item.position = position;
                moved = true;
            }
        });
        if !moved {
            return self.finish();
        }
        // The item takes the zone's place on screen.
        let anchor = rect_of("ul.items > .drop-zone").map(|z| z.top());
        self.end(moving, anchor);
    }
}

/// While moving, stops the page scrolling by itself, so swipes and the
/// wheel can move the zone instead; the page then scrolls only to follow
/// the zone. Also stops the browser's scroll anchoring, which would fight
/// that scrolling.
fn lock_page(lock: bool) {
    for (property, value) in [
        ("overflow", "hidden"),
        ("touch-action", "none"),
        ("overflow-anchor", "none"),
    ] {
        set_style("html", property, lock.then_some(value));
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

/// The rows other than `moving` (if any), top to bottom, with their tops and
/// bottoms on screen as if the drop zone weren't in the list. Leaving the
/// zone out keeps it from pushing the rows it's measured against, which
/// would flip it between two gaps.
fn layout<'a>(moving: impl Into<Option<&'a str>>) -> Vec<(String, f64, f64)> {
    let moving = moving.into();
    let zone = document()
        .query_selector("ul.items > .drop-zone")
        .ok()
        .flatten()
        .map(|z| z.get_bounding_client_rect());
    let (zone_top, zone_height) = zone.map_or((f64::INFINITY, 0.0), |z| (z.top(), z.height()));
    rows()
        .into_iter()
        .filter(|(id, _)| Some(id.as_str()) != moving)
        .map(|(id, row)| {
            let rect = row.get_bounding_client_rect();
            let shift = if rect.top() >= zone_top {
                zone_height
            } else {
                0.0
            };
            (id, rect.top() - shift, rect.bottom() - shift)
        })
        .collect()
}

fn viewport_height() -> f64 {
    let height = window().inner_height().ok().and_then(|h| h.as_f64());
    height.unwrap_or(0.0)
}

fn scroll_y() -> f64 {
    window().scroll_y().unwrap_or(0.0)
}

fn max_scroll() -> f64 {
    let height = document()
        .document_element()
        .map_or(0, |e| e.scroll_height());
    (f64::from(height) - viewport_height()).max(0.0)
}

fn rect_of(selector: &str) -> Option<web_sys::DomRect> {
    let el = document().query_selector(selector).ok().flatten()?;
    Some(el.get_bounding_client_rect())
}

/// Sets (or with `None` clears) room below the rows.
fn set_room(px: Option<f64>) {
    let px = px.map(|px| format!("{px}px"));
    set_style("ul.items", "padding-bottom", px.as_deref());
}

fn set_style(selector: &str, property: &str, value: Option<&str>) {
    let Ok(Some(el)) = document().query_selector(selector) else {
        return;
    };
    let style = el.unchecked_into::<HtmlElement>().style();
    let _ = match value {
        Some(value) => style.set_property(property, value),
        None => style.remove_property(property).map(|_| ()),
    };
}

/// Scrolls so the row for `item_id` has its top at `anchor` on screen.
fn keep(item_id: &str, anchor: f64) {
    if let Some(row) = row(item_id) {
        let top = row.get_bounding_client_rect().top();
        window().scroll_by_with_x_and_y(0.0, top - anchor);
    }
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
    let tap = {
        let item_id = item_id.clone();
        move |ev: ev::MouseEvent| mover.tap(&item_id, f64::from(ev.client_y()))
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
            on:click=tap
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
