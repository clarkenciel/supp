//! Installable-app plumbing: the service worker (`sw.js`) that lets the app
//! start with no network, and switching to new versions of the app.
//!
//! A new build arrives as a new worker, which installs in the background and
//! then waits: the running page keeps the build it started with until the
//! user accepts the update prompt.

use std::time::Duration;

use futures::{FutureExt, channel::mpsc, select, stream::StreamExt};
use gloo_timers::future::sleep;
use leptos::{logging, prelude::*};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    EventTarget, ServiceWorker, ServiceWorkerContainer, ServiceWorkerRegistration,
    ServiceWorkerState,
};

/// Whether a new version of the app has installed and is waiting.
#[derive(Clone, Copy)]
pub struct NewVersion(pub RwSignal<bool>);

/// How long to wait for a new version to install before reloading anyway.
const UPDATE_TIMEOUT: Duration = Duration::from_secs(30);

/// The service worker API, if this browser has it here. It is missing
/// outside secure contexts and in some private modes.
fn container() -> Option<ServiceWorkerContainer> {
    let navigator = window().navigator();
    js_sys::Reflect::has(&navigator, &JsValue::from_str("serviceWorker"))
        .unwrap_or(false)
        .then(|| navigator.service_worker())
}

/// Registers the service worker, which precaches this build's files.
pub async fn register() {
    let Some(container) = container() else {
        return;
    };
    if let Err(err) = JsFuture::from(container.register("/sw.js")).await {
        logging::warn!("registering the service worker: {err:?}");
    }
}

/// Sets `ready` whenever a new version has installed and is waiting.
pub async fn watch_for_updates(ready: RwSignal<bool>) {
    let Some(container) = container() else {
        return;
    };
    // Resolves once a worker is active, i.e. after the first install.
    let Ok(promise) = container.ready() else {
        return;
    };
    let Ok(registration) = JsFuture::from(promise).await else {
        return;
    };
    let registration: ServiceWorkerRegistration = registration.unchecked_into();
    loop {
        if let Some(worker) = registration.installing() {
            wait_for_state(&worker, |s| s != ServiceWorkerState::Installing).await;
        }
        // With no controller this page predates the worker (first visit):
        // there is no older version to replace.
        if registration.waiting().is_some() && container.controller().is_some() {
            ready.set(true);
        }
        next_event(&registration, "updatefound").await;
    }
}

/// Switches to the newest version of the app and reloads. Activates the
/// waiting worker if there is one; otherwise (the server refused this
/// version before the browser noticed the new one) checks for one first. A
/// plain reload would only bring back the cached build.
pub async fn update_and_reload() {
    select! {
        _ = activate_newest().fuse() => {}
        _ = sleep(UPDATE_TIMEOUT).fuse() => logging::warn!("timed out updating the app"),
    }
    let _ = window().location().reload();
}

async fn activate_newest() {
    let Some(container) = container() else {
        return;
    };
    let Ok(registration) = JsFuture::from(container.get_registration()).await else {
        return;
    };
    let Ok(registration) = registration.dyn_into::<ServiceWorkerRegistration>() else {
        return;
    };
    if registration.waiting().is_none() {
        // Resolves after the browser has fetched sw.js and, if it changed,
        // started installing it.
        let Ok(update) = registration.update() else {
            return;
        };
        if let Err(err) = JsFuture::from(update).await {
            logging::warn!("checking for a new version: {err:?}");
            return;
        }
        if let Some(worker) = registration.installing() {
            wait_for_state(&worker, |s| s != ServiceWorkerState::Installing).await;
        }
    }
    let Some(worker) = registration.waiting() else {
        return;
    };
    // sw.js calls skipWaiting() on this message.
    let _ = worker.post_message(&JsValue::from_str("skipWaiting"));
    wait_for_state(&worker, |s| {
        matches!(
            s,
            ServiceWorkerState::Activated | ServiceWorkerState::Redundant
        )
    })
    .await;
}

async fn wait_for_state(worker: &ServiceWorker, done: impl Fn(ServiceWorkerState) -> bool) {
    while !done(worker.state()) {
        next_event(worker, "statechange").await;
    }
}

/// Resolves on the next `name` event on `target`.
async fn next_event(target: &EventTarget, name: &str) {
    let (tx, mut rx) = mpsc::unbounded::<()>();
    let listener = Listener::new(target, name, move || {
        let _ = tx.unbounded_send(());
    });
    rx.next().await;
    drop(listener);
}

/// An event listener that is removed when dropped, so a cancelled wait
/// leaves no callback behind.
struct Listener {
    target: EventTarget,
    name: String,
    callback: Closure<dyn FnMut()>,
}

impl Listener {
    fn new(target: &EventTarget, name: &str, callback: impl FnMut() + 'static) -> Self {
        let callback = Closure::<dyn FnMut()>::new(callback);
        let _ = target.add_event_listener_with_callback(name, callback.as_ref().unchecked_ref());
        Self {
            target: target.clone(),
            name: name.to_string(),
            callback,
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.target.remove_event_listener_with_callback(
            &self.name,
            self.callback.as_ref().unchecked_ref(),
        );
    }
}
