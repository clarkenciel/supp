//! Identity providers: how a login proves who someone is.
//!
//! The auth routes only see this trait, so Google (production) and the dev
//! provider (local testing) are interchangeable. CSRF protection (`state`)
//! is handled by the routes, not by each provider.

use std::collections::HashMap;

use futures_util::future::BoxFuture;

use crate::accounts::Identity;

/// The first leg of a login.
pub struct Begin {
    /// Where to send the browser.
    pub redirect: String,
    /// Provider state to hand back to [`IdentityProvider::complete`] (kept in
    /// an encrypted cookie until the callback), e.g. a PKCE verifier.
    pub pending: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("the identity provider rejected the login: {0}")]
    Rejected(String),
    #[error("identity provider unavailable: {0}")]
    Unavailable(String),
}

pub trait IdentityProvider: Send + Sync {
    /// Starts a login. `state` must round-trip to the callback untouched;
    /// `redirect_uri` is where the provider sends the browser back.
    fn begin<'a>(
        &'a self,
        state: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Begin, ProviderError>>;

    /// Finishes a login from the callback's query parameters.
    fn complete<'a>(
        &'a self,
        pending: &'a str,
        params: &'a HashMap<String, String>,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>>;

    /// Whether `/auth/dev` (the dev provider's sign-in form) is served.
    fn is_dev(&self) -> bool {
        false
    }
}

/// Local-testing provider: anyone can sign in as any name. Never enable it
/// on a reachable server.
pub struct DevProvider;

impl IdentityProvider for DevProvider {
    fn begin<'a>(
        &'a self,
        state: &'a str,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Begin, ProviderError>> {
        Box::pin(async move {
            Ok(Begin {
                redirect: format!("/auth/dev?state={state}"),
                pending: String::new(),
            })
        })
    }

    fn complete<'a>(
        &'a self,
        _pending: &'a str,
        params: &'a HashMap<String, String>,
        _redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let name = params
                .get("code")
                .map(|n| n.trim())
                .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric()))
                .ok_or_else(|| ProviderError::Rejected("name must be letters and digits".into()))?;
            Ok(Identity {
                subject: format!("dev:{name}"),
                email: format!("{name}@dev.invalid"),
                name: name.to_string(),
            })
        })
    }

    fn is_dev(&self) -> bool {
        true
    }
}

/// The dev provider's sign-in form, served at `/auth/dev`.
pub fn dev_sign_in_page(state: &str) -> String {
    let state: String = state
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let body = format!(
        r#"<div class="notice"><p>Development identity provider. Any name signs in as <code>dev:&lt;name&gt;</code>.</p></div>
  <form class="auth-form" action="/auth/callback" method="get">
    <input type="hidden" name="state" value="{state}">
    <label>Name <input name="code" type="text" pattern="[A-Za-z0-9]+" autocomplete="username" required autofocus></label>
    <button type="submit">Sign in</button>
  </form>"#
    );
    super::page::page("Sign in", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(code: &str) -> HashMap<String, String> {
        HashMap::from([("code".to_string(), code.to_string())])
    }

    #[tokio::test]
    async fn dev_provider_round_trip() {
        let begin = DevProvider.begin("abc", "unused").await.unwrap();
        assert_eq!(begin.redirect, "/auth/dev?state=abc");
        let identity = DevProvider
            .complete("", &params("alice"), "unused")
            .await
            .unwrap();
        assert_eq!(identity.subject, "dev:alice");
    }

    #[tokio::test]
    async fn dev_provider_rejects_odd_names() {
        for bad in ["", " ", "a:b", "<script>"] {
            assert!(DevProvider.complete("", &params(bad), "").await.is_err());
        }
    }

    #[test]
    fn dev_page_escapes_state() {
        assert!(!dev_sign_in_page(r#""><script>"#).contains("<script>"));
    }
}
