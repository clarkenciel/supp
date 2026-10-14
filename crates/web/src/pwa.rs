//! Installable-app plumbing: the service worker (`sw.js`) that lets the app
//! start with no network.

use leptos::{logging, prelude::*};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::ServiceWorkerRegistration;

/// Registers the service worker, which precaches this build's files.
/// `None` if the browser has no service workers or registration failed.
pub async fn register() -> Option<ServiceWorkerRegistration> {
    let navigator = window().navigator();
    // Missing outside secure contexts, and in some private modes.
    if !js_sys::Reflect::has(&navigator, &JsValue::from_str("serviceWorker")).unwrap_or(false) {
        return None;
    }
    match JsFuture::from(navigator.service_worker().register("/sw.js")).await {
        Ok(registration) => Some(registration.unchecked_into()),
        Err(err) => {
            logging::warn!("registering the service worker: {err:?}");
            None
        }
    }
}
