//! supp server: Automerge sync over WebSocket, persisted to SQLite.

pub mod accounts;
pub mod connection;
pub mod docs;
pub mod store;

use std::sync::Arc;

use axum::{Router, routing::get};

pub use docs::Docs;
pub use store::Store;

#[derive(Clone)]
pub struct AppState {
    pub docs: Arc<Docs>,
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/sync", get(connection::handler))
        .with_state(state)
}
