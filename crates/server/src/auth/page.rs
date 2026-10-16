//! The server's own sign-in pages, built from the components the web client
//! uses (`supp-ui`) and rendered to HTML here. The client's stylesheet is
//! compiled in, so they look the same without the client having been built.

use leptos::prelude::*;
use supp_ui::Brand;

/// The client's stylesheet, as of this build.
const STYLE: &str = include_str!("../../../web/style.css");

/// A whole page: the brand, a title bar, then `body`.
pub fn page(title: &str, body: impl IntoView + 'static) -> String {
    let main = view! {
        <main class="auth">
            <Brand />
            <header>
                <h1>{title.to_string()}</h1>
            </header>
            {body}
        </main>
    }
    .to_html();
    // The head is written out rather than rendered: the stylesheet must go
    // in verbatim, and Leptos would escape it as text.
    format!(
        r##"<!doctype html>
<html lang="en">
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#fafaf9" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#1c1917" media="(prefers-color-scheme: dark)">
<link rel="icon" type="image/svg+xml" href="/icons/icon.svg">
<title>{title} · supp</title>
<style>{STYLE}</style>
{main}
</html>
"##,
        title = escape(title),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
