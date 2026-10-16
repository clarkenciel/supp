//! The look of the server's own sign-in pages: the web client's stylesheet,
//! compiled in, so they share its tokens and components without depending
//! on the client having been built.

/// The client's stylesheet, as of this build.
const STYLE: &str = include_str!("../../../web/style.css");

/// The app's mark: the add button's pebble with a tick.
const MARK: &str = r##"<svg class="mark" viewBox="0 0 64 64" aria-hidden="true"><path d="M14 20C19 8 40 5 50 12C60 19 60 38 52 48C44 58 22 59 13 50C5 42 9 30 14 20Z" transform="translate(3 3)" fill="#f472b6"/><path d="M14 20C19 8 40 5 50 12C60 19 60 38 52 48C44 58 22 59 13 50C5 42 9 30 14 20Z" fill="#facc15" stroke="currentColor" stroke-width="3" stroke-linejoin="round"/><path d="M21 33l8 8 15-16" fill="none" stroke="#0c0a09" stroke-width="5.5" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;

/// A whole page: the app's mark and name, a heading, then `body`, which
/// must already be escaped.
pub fn page(title: &str, body: &str) -> String {
    format!(
        r##"<!doctype html>
<html lang="en">
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="theme-color" content="#fafaf9" media="(prefers-color-scheme: light)">
<meta name="theme-color" content="#1c1917" media="(prefers-color-scheme: dark)">
<link rel="icon" type="image/svg+xml" href="/icons/icon.svg">
<title>{title} · supp</title>
<style>{STYLE}</style>
<main class="auth">
  <p class="brand">{MARK}supp</p>
  <header><h1>{title}</h1></header>
  {body}
</main>
</html>
"##
    )
}

/// Escapes text for HTML content and attribute values.
pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
