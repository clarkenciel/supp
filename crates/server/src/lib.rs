//! supp server: Automerge sync over WebSocket, persisted to SQLite.

pub mod accounts;
pub mod admin;
pub mod auth;
pub mod connection;
pub mod docs;
pub mod store;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    extract::FromRef,
    routing::{get, post},
};
use axum_extra::extract::cookie::Key;
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
        .route("/api/me", get(auth::routes::me));
    if state.auth.provider.is_dev() {
        router = router.route("/auth/dev", get(auth::routes::dev_sign_in));
    }
    if let Some(dir) = &state.static_dir {
        // Unknown paths get index.html so client-side routes (/lists/…)
        // survive a reload or a shared link.
        let assets = ServeDir::new(dir).fallback(ServeFile::new(dir.join("index.html")));
        router = router.fallback_service(assets);
    }
    router.with_state(state)
}
