//! Google as an OpenID Connect identity provider: authorization code flow
//! with PKCE, ID token verified (signature, issuer, audience, expiry, nonce)
//! by the openidconnect crate.

use std::collections::HashMap;

use futures_util::future::BoxFuture;
use openidconnect::{
    AuthenticationFlow, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet,
    EndpointNotSet, EndpointSet, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
    core::{CoreClient, CoreProviderMetadata, CoreResponseType},
    reqwest,
};
use serde::{Deserialize, Serialize};

use super::provider::{Begin, IdentityProvider, ProviderError};
use crate::accounts::Identity;

const ISSUER: &str = "https://accounts.google.com";

type Client = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

pub struct GoogleProvider {
    client_id: ClientId,
    client_secret: ClientSecret,
    http: reqwest::Client,
}

#[derive(Serialize, Deserialize)]
struct Pending {
    pkce_verifier: String,
    nonce: String,
}

impl GoogleProvider {
    pub fn new(client_id: String, client_secret: String) -> anyhow::Result<Self> {
        let http = reqwest::ClientBuilder::new()
            // Following redirects would open the client to SSRF.
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client_id: ClientId::new(client_id),
            client_secret: ClientSecret::new(client_secret),
            http,
        })
    }

    /// Discovers Google's endpoints and signing keys. Done per login rather
    /// than once at startup so key rotation never breaks verification;
    /// logins are rare enough that the two extra requests don't matter.
    async fn client(&self, redirect_uri: &str) -> Result<Client, ProviderError> {
        let issuer = IssuerUrl::new(ISSUER.to_string()).expect("valid issuer URL");
        let metadata = CoreProviderMetadata::discover_async(issuer, &self.http)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("discovery: {e}")))?;
        let redirect = RedirectUrl::new(redirect_uri.to_string())
            .map_err(|e| ProviderError::Unavailable(format!("redirect URI: {e}")))?;
        Ok(CoreClient::from_provider_metadata(
            metadata,
            self.client_id.clone(),
            Some(self.client_secret.clone()),
        )
        .set_redirect_uri(redirect))
    }
}

impl IdentityProvider for GoogleProvider {
    fn begin<'a>(
        &'a self,
        state: &'a str,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Begin, ProviderError>> {
        Box::pin(async move {
            let client = self.client(redirect_uri).await?;
            let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
            let state = state.to_string();
            let (url, _state, nonce) = client
                .authorize_url(
                    AuthenticationFlow::<CoreResponseType>::AuthorizationCode,
                    move || CsrfToken::new(state),
                    Nonce::new_random,
                )
                .add_scope(Scope::new("email".into()))
                .add_scope(Scope::new("profile".into()))
                .set_pkce_challenge(challenge)
                .url();
            let pending = Pending {
                pkce_verifier: verifier.secret().clone(),
                nonce: nonce.secret().clone(),
            };
            Ok(Begin {
                redirect: url.to_string(),
                pending: serde_json::to_string(&pending).expect("serializable"),
            })
        })
    }

    fn complete<'a>(
        &'a self,
        pending: &'a str,
        params: &'a HashMap<String, String>,
        redirect_uri: &'a str,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            if let Some(error) = params.get("error") {
                return Err(ProviderError::Rejected(error.clone()));
            }
            let code = params
                .get("code")
                .ok_or_else(|| ProviderError::Rejected("missing code".into()))?;
            let pending: Pending = serde_json::from_str(pending)
                .map_err(|_| ProviderError::Rejected("bad login state".into()))?;

            let client = self.client(redirect_uri).await?;
            let response = client
                .exchange_code(AuthorizationCode::new(code.clone()))
                .map_err(|e| ProviderError::Unavailable(format!("token endpoint: {e}")))?
                .set_pkce_verifier(PkceCodeVerifier::new(pending.pkce_verifier))
                .request_async(&self.http)
                .await
                .map_err(|e| ProviderError::Rejected(format!("code exchange: {e}")))?;
            let id_token = response
                .id_token()
                .ok_or_else(|| ProviderError::Rejected("no ID token".into()))?;
            let claims = id_token
                .claims(&client.id_token_verifier(), &Nonce::new(pending.nonce))
                .map_err(|e| ProviderError::Rejected(format!("ID token: {e}")))?;

            let email = claims.email().map(|e| e.to_string()).unwrap_or_default();
            let name = claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.to_string())
                .unwrap_or_else(|| email.clone());
            Ok(Identity {
                subject: format!("google:{}", claims.subject().as_str()),
                email,
                name,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs network access to accounts.google.com: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn begin_builds_a_pkce_authorization_url() {
        let provider = GoogleProvider::new("client-id".into(), "secret".into()).unwrap();
        let begin = provider
            .begin("the-state", "https://supp.test/auth/callback")
            .await
            .unwrap();
        let url = reqwest::Url::parse(&begin.redirect).unwrap();
        assert_eq!(url.host_str(), Some("accounts.google.com"));
        let query: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["state"], "the-state");
        assert_eq!(query["client_id"], "client-id");
        assert_eq!(query["redirect_uri"], "https://supp.test/auth/callback");
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(query["scope"].contains("openid"));
        let pending: Pending = serde_json::from_str(&begin.pending).unwrap();
        assert_eq!(query["nonce"], pending.nonce);
    }
}
