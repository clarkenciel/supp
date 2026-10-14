//! Resolving the session cookie to a user.

use axum::{
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
};
use axum_extra::extract::CookieJar;

use crate::{
    AppState,
    accounts::{Token, User},
};

pub const SESSION_COOKIE: &str = "supp_session";

/// Extracts the signed-in user, or rejects with 401.
pub struct AuthUser(pub User);

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, app: &AppState) -> Result<Self, StatusCode> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar
            .get(SESSION_COOKIE)
            .map(|c| Token::from(c.value().to_string()))
            .ok_or(StatusCode::UNAUTHORIZED)?;
        match app.accounts.session_user(&token).await {
            Ok(Some(user)) => Ok(Self(user)),
            Ok(None) => Err(StatusCode::UNAUTHORIZED),
            Err(err) => {
                tracing::error!("session lookup: {err}");
                Err(StatusCode::INTERNAL_SERVER_ERROR)
            }
        }
    }
}
