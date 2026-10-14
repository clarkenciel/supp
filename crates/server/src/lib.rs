//! supp server: Automerge sync over WebSocket, persisted to SQLite.

pub mod accounts;
pub mod admin;
pub mod api;
pub mod auth;
pub mod connection;
pub mod docs;
pub mod store;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{FromRef, Request, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use axum_extra::extract::cookie::Key;
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

pub use docs::Docs;
pub use store::Store;

use crate::{accounts::Accounts, auth::AuthConfig};

#[derive(Clone)]
pub struct AppState {
    pub docs: Arc<Docs>,
    pub accounts: Accounts,
    pub auth: Arc<AuthConfig>,
    /// The built web client, if this server serves it.
    pub static_dir: Option<PathBuf>,
    cookie_key: Key,
}

impl AppState {
    pub async fn new(store: &Store, docs: Arc<Docs>, auth: AuthConfig) -> anyhow::Result<Self> {
        Ok(Self {
            docs,
            accounts: store.accounts(),
            auth: Arc::new(auth),
            static_dir: None,
            cookie_key: Key::from(&store.server_key("cookie").await?),
        })
    }
}

impl FromRef<AppState> for Key {
    fn from_ref(app: &AppState) -> Key {
        app.cookie_key.clone()
    }
}

pub fn app(state: AppState) -> Router {
    let mut router = Router::new()
        .route("/sync", get(connection::handler))
        .route("/auth/login", get(auth::routes::login))
        .route("/auth/callback", get(auth::routes::callback))
        .route("/auth/logout", post(auth::routes::logout))
        .route("/join/{token}", get(auth::routes::join))
        .route("/api/me", get(auth::routes::me))
        .route(
            "/api/lists/{doc_id}/invites",
            get(api::share_links).post(api::create_share_link),
        )
        .route(
            "/api/lists/{doc_id}/invites/{id}",
            delete(api::revoke_share_link),
        );
    if state.auth.provider.is_dev() {
        router = router.route("/auth/dev", get(auth::routes::dev_sign_in));
    }
    if state.static_dir.is_some() {
        router = router.fallback(static_files);
    }
    router.with_state(state)
}

/// Serves the built web client. Client routes (paths whose last segment has
/// no extension, like /lists/…) get index.html so they survive a reload or
/// a shared link. A missing file is a 404 rather than index.html, so a stale
/// asset URL can never be cached as a long-lived copy of the page.
async fn static_files(State(app): State<AppState>, request: Request) -> Response {
    let Some(dir) = &app.static_dir else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let file = request.uri().path().rsplit('/').next().unwrap_or_default();
    let is_route = !file.contains('.');
    let cache_control = if !is_route && is_hashed(file) {
        // trunk names these after their contents.
        "public, max-age=31536000, immutable"
    } else {
        // index.html, sw.js, the manifest and icons keep their names across
        // builds, so the browser must revalidate them (a cheap 304). The
        // service worker serves them from its own cache anyway.
        "no-cache"
    };
    let served = if is_route {
        let index = ServeFile::new(dir.join("index.html"));
        ServeDir::new(dir).fallback(index).oneshot(request).await
    } else {
        ServeDir::new(dir).oneshot(request).await
    };
    let mut response = match served {
        Ok(response) => response.map(Body::new),
        Err(infallible) => match infallible {},
    };
    if response.status().is_success() || response.status() == StatusCode::NOT_MODIFIED {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(cache_control),
        );
    }
    response
}

/// Whether a file name carries trunk's content hash, e.g.
/// `supp-web-374097c3cc8cad86_bg.wasm`.
fn is_hashed(file: &str) -> bool {
    file.split(['-', '_', '.']).any(|part| {
        part.len() == 16 && part.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}
