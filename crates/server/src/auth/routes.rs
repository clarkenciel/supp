//! Login, invite links, logout and the signed-in user.
//!
//! - `GET /auth/login` and `GET /join/{token}` start a login (the latter
//!   carrying an invite), remembering CSRF state, provider state and the
//!   invite in a short-lived encrypted cookie.
//! - `GET /auth/callback` checks the state, asks the provider for the
//!   identity, logs in (redeeming the invite if needed) and sets the session
//!   cookie.
//! - `POST /auth/logout` ends the session; `GET /api/me` reports the user.

use std::collections::HashMap;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::{
    CookieJar, PrivateCookieJar,
    cookie::{Cookie, SameSite},
};
use serde::{Deserialize, Serialize};
use supp_shared::proto::api::v1::Me;

use super::{
    provider::{ProviderError, dev_sign_in_page},
    session::{AuthUser, SESSION_COOKIE},
};
use crate::{
    AppState,
    accounts::{LoginError, SESSION_TTL, Token},
    api::Protobuf,
};

const LOGIN_COOKIE: &str = "supp_login";
const LOGIN_TTL_SECS: i64 = 10 * 60;

/// What a login needs to remember between redirect and callback.
#[derive(Serialize, Deserialize)]
struct PendingLogin {
    state: String,
    pending: String,
    invite: Option<String>,
}

pub async fn login(State(app): State<AppState>, jar: PrivateCookieJar) -> Response {
    start(app, jar, None).await
}

pub async fn join(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    Path(token): Path<String>,
) -> Response {
    start(app, jar, Some(token)).await
}

async fn start(app: AppState, jar: PrivateCookieJar, invite: Option<String>) -> Response {
    let state = Token::generate();
    let begin = match app
        .auth
        .provider
        .begin(state.as_str(), &app.auth.redirect_uri())
        .await
    {
        Ok(begin) => begin,
        Err(err) => return error_page(StatusCode::BAD_GATEWAY, &err.to_string()),
    };
    let pending = PendingLogin {
        state: state.as_str().to_string(),
        pending: begin.pending,
        invite,
    };
    let cookie = Cookie::build((LOGIN_COOKIE, serde_json::to_string(&pending).unwrap()))
        .path("/auth")
        .http_only(true)
        .secure(app.auth.secure_cookies)
        // Lax: the callback is a top-level navigation back from the provider.
        .same_site(SameSite::Lax)
        .max_age(time_secs(LOGIN_TTL_SECS));
    (jar.add(cookie), Redirect::to(&begin.redirect)).into_response()
}

pub async fn callback(
    State(app): State<AppState>,
    jar: PrivateCookieJar,
    session_jar: CookieJar,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(pending) = jar
        .get(LOGIN_COOKIE)
        .and_then(|c| serde_json::from_str::<PendingLogin>(c.value()).ok())
    else {
        return error_page(
            StatusCode::BAD_REQUEST,
            "This sign-in link has expired. Try again.",
        );
    };
    let jar = jar.remove(Cookie::build(LOGIN_COOKIE).path("/auth"));
    if params.get("state") != Some(&pending.state) {
        return (
            jar,
            error_page(
                StatusCode::BAD_REQUEST,
                "Sign-in state mismatch. Try again.",
            ),
        )
            .into_response();
    }

    let identity = match app
        .auth
        .provider
        .complete(&pending.pending, &params, &app.auth.redirect_uri())
        .await
    {
        Ok(identity) => identity,
        Err(err @ ProviderError::Rejected(_)) => {
            return (jar, error_page(StatusCode::UNAUTHORIZED, &err.to_string())).into_response();
        }
        Err(err @ ProviderError::Unavailable(_)) => {
            return (jar, error_page(StatusCode::BAD_GATEWAY, &err.to_string())).into_response();
        }
    };

    let invite = pending.invite.map(Token::from);
    let login = match app.accounts.login(&identity, invite.as_ref()).await {
        Ok(login) => login,
        Err(LoginError::InviteRequired) => {
            let message = "You need an invite to create an account. Ask whoever runs this \
                           server, or a friend with a list to share, for a link.";
            return (jar, error_page(StatusCode::FORBIDDEN, message)).into_response();
        }
        Err(LoginError::Disabled) => {
            let message = "This account has been disabled.";
            return (jar, error_page(StatusCode::FORBIDDEN, message)).into_response();
        }
        Err(LoginError::Db(err)) => {
            tracing::error!("login: {err}");
            return (
                jar,
                error_page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed."),
            )
                .into_response();
        }
    };
    let token = match app.accounts.create_session(login.user.id).await {
        Ok(token) => token,
        Err(err) => {
            tracing::error!("creating session: {err}");
            return (
                jar,
                error_page(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in failed."),
            )
                .into_response();
        }
    };
    let session = Cookie::build((SESSION_COOKIE, token.as_str().to_string()))
        .path("/")
        .http_only(true)
        .secure(app.auth.secure_cookies)
        .same_site(SameSite::Lax)
        .max_age(time_secs(SESSION_TTL.as_secs() as i64));
    // `joined` tells the client this list is wanted again, in case it was
    // deleted on this device and the leave hasn't reached the server yet.
    let to = match login.joined {
        Some(doc) => format!("/lists/{doc}?joined=1"),
        None => "/".to_string(),
    };
    (jar, session_jar.add(session), Redirect::to(&to)).into_response()
}

pub async fn logout(State(app): State<AppState>, jar: CookieJar) -> Response {
    if let Some(cookie) = jar.get(SESSION_COOKIE)
        && let Err(err) = app
            .accounts
            .delete_session(&Token::from(cookie.value().to_string()))
            .await
    {
        tracing::error!("logout: {err}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let jar = jar.remove(Cookie::build(SESSION_COOKIE).path("/"));
    (jar, StatusCode::NO_CONTENT).into_response()
}

pub async fn me(AuthUser(user): AuthUser) -> Protobuf<Me> {
    Protobuf(Me {
        user_id: user.id.0,
        email: user.email,
        name: user.name,
    })
}

pub async fn dev_sign_in(Query(params): Query<HashMap<String, String>>) -> Html<String> {
    Html(dev_sign_in_page(params.get("state").map_or("", |s| s)))
}

fn error_page(status: StatusCode, message: &str) -> Response {
    let body = format!(
        r#"<div class="notice" role="alert"><p>{}</p></div>
  <a class="button secondary" href="/">Back to your lists</a>"#,
        super::page::escape(message)
    );
    (status, Html(super::page::page("Couldn't sign you in", &body))).into_response()
}

fn time_secs(secs: i64) -> time::Duration {
    time::Duration::seconds(secs)
}
