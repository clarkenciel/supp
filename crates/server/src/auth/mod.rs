//! Authentication: identity providers, login routes and sessions.

pub mod provider;
pub mod routes;
pub mod session;

use std::sync::Arc;

pub use provider::{DevProvider, IdentityProvider};
pub use session::AuthUser;

pub struct AuthConfig {
    pub provider: Arc<dyn IdentityProvider>,
    /// Public base URL, e.g. `https://lists.example.com`.
    pub public_url: String,
    /// Mark cookies `Secure`; true whenever the public URL is https.
    pub secure_cookies: bool,
}

impl AuthConfig {
    pub fn new(provider: Arc<dyn IdentityProvider>, public_url: &str) -> Self {
        let public_url = public_url.trim_end_matches('/').to_string();
        Self {
            provider,
            secure_cookies: public_url.starts_with("https://"),
            public_url,
        }
    }

    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/callback", self.public_url)
    }
}
