//! Components shared by the web client, which renders them in the browser,
//! and the server, which renders them to HTML for its own pages. Their look
//! comes from `crates/web/style.css`, which both sides load.

use leptos::prelude::*;

/// The outline of the pebble: hand-drawn and a little lopsided, in a 64×64
/// box.
const PEBBLE: &str = "M14 20C19 8 40 5 50 12C60 19 60 38 52 48C44 58 22 59 13 50C5 42 9 30 14 20Z";

/// The app's signature shape: a yellow Memphis pebble with an ink outline
/// and a hard pink shadow, drawn in a 64×64 box. `children` are SVG marks
/// laid on top (the add button's `+`, the app mark's tick), drawn in the
/// same box and given the `glyph` class for their ink stroke.
///
/// Decorative: it is hidden from screen readers, so whatever holds it
/// names it.
#[component]
pub fn Pebble(
    /// Extra classes for the `<svg>`, for sizing where it's used.
    #[prop(optional, into)]
    class: String,
    children: Children,
) -> impl IntoView {
    view! {
        <svg class=format!("pebble {class}") viewBox="0 0 64 64" aria-hidden="true">
            <path class="shadow" transform="translate(4 4)" d=PEBBLE />
            <path class="face" d=PEBBLE />
            {children()}
        </svg>
    }
}

/// The app mark: the pebble with a tick, as on the app icon.
#[component]
pub fn AppMark() -> impl IntoView {
    view! {
        <Pebble class="mark">
            <path class="glyph" d="M21 33l8 8 15-16" />
        </Pebble>
    }
}

/// The mark and the app's name, for pages outside the app itself.
#[component]
pub fn Brand() -> impl IntoView {
    view! { <p class="brand"><AppMark />"supp"</p> }
}

/// A line of text saying how things stand: loading, missing, or gone
/// wrong. An `error` is announced at once (`role="alert"`); `live` text is
/// announced when it changes (`role="status"`); otherwise it's just read in
/// place.
#[component]
pub fn Status(
    #[prop(optional)] error: bool,
    #[prop(optional)] live: bool,
    children: Children,
) -> impl IntoView {
    let role = if error {
        Some("alert")
    } else if live {
        Some("status")
    } else {
        None
    };
    view! {
        <p class="status" class:error=error role=role>
            {children()}
        </p>
    }
}

/// An outlined box for something that needs the person: what happened, then
/// the action that fixes it. An `alert` is announced at once; otherwise it's
/// a polite status.
#[component]
pub fn Notice(#[prop(optional)] alert: bool, children: Children) -> impl IntoView {
    view! {
        <div class="notice" role=if alert { "alert" } else { "status" }>
            {children()}
        </div>
    }
}
