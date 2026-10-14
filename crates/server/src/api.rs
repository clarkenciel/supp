//! The HTTP API. Bodies are protobuf (`application/x-protobuf`, messages in
//! `api.proto`), and every route needs a session.
//!
//! - `GET /api/me` (in [`auth::routes`](crate::auth::routes)).
//! - `POST /api/lists/{doc_id}/invites` creates a share link for a list.
//! - `GET /api/lists/{doc_id}/invites` lists its live share links.
//! - `DELETE /api/lists/{doc_id}/invites/{id}` revokes one.
//!
//! The share-link routes need active membership of the list; for anyone
//! else the list doesn't exist (404). Any member may revoke any of the
//! list's links, not just their own: any member can create links and change
//! everything in the list anyway, and a link posted in the wrong place should
//! die as soon as any member notices, not when its creator does.
//!
//! Routes that change state are never GET: the session cookie is
//! `SameSite=Lax`, which still sends it on cross-site top-level GETs.

use std::{fmt::Display, time::Duration};

use axum::{
    body::Bytes,
    extract::{FromRequest, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use prost::Message;
use supp_shared::{
    DocId,
    proto::api::v1::{CreateShareLink, CreatedShareLink, ShareLink, ShareLinks},
};

use crate::{
    AppState,
    accounts::{Access, InviteId, NewInvite},
    admin::join_url,
    auth::AuthUser,
};

pub const CONTENT_TYPE: &str = "application/x-protobuf";

/// Share link limits. The defaults are deliberately tight: a link is a
/// bearer token for a list and an account, and is easily forwarded.
pub const DEFAULT_LINK_USES: u32 = 1;
pub const MAX_LINK_USES: u32 = 100;
pub const DEFAULT_LINK_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);
pub const MAX_LINK_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A protobuf request or response body.
pub struct Protobuf<T>(pub T);

impl<T: Message> IntoResponse for Protobuf<T> {
    fn into_response(self) -> Response {
        (
            [(header::CONTENT_TYPE, CONTENT_TYPE)],
            self.0.encode_to_vec(),
        )
            .into_response()
    }
}

impl<T: Message + Default, S: Send + Sync> FromRequest<S> for Protobuf<T> {
    type Rejection = StatusCode;

    async fn from_request(request: Request, state: &S) -> Result<Self, StatusCode> {
        // A cross-site HTML form can't send this content type, so this also
        // backs up SameSite against form-based CSRF.
        let is_protobuf = request
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v == CONTENT_TYPE);
        if !is_protobuf {
            return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        T::decode(bytes)
            .map(Self)
            .map_err(|_| StatusCode::BAD_REQUEST)
    }
}

pub async fn create_share_link(
    State(app): State<AppState>,
    AuthUser(user): AuthUser,
    Path(doc): Path<String>,
    Protobuf(request): Protobuf<CreateShareLink>,
) -> Result<Protobuf<CreatedShareLink>, StatusCode> {
    let doc = parse_doc(&doc)?;
    let max_uses = match request.max_uses {
        0 => DEFAULT_LINK_USES,
        n if n <= MAX_LINK_USES => n,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let ttl = match Duration::from_secs(request.expires_in_secs.into()) {
        Duration::ZERO => DEFAULT_LINK_TTL,
        ttl if ttl <= MAX_LINK_TTL => ttl,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    // The same check as subscribing, so a list made moments ago can be
    // shared before its first sync has claimed it.
    match app
        .accounts
        .authorize(user.id, doc)
        .await
        .map_err(internal)?
    {
        Access::Member | Access::Claimed => {}
        Access::NotMember | Access::Left => return Err(StatusCode::NOT_FOUND),
    }
    let invite = NewInvite {
        doc_id: Some(doc),
        created_by: Some(user.id),
        note: String::new(),
        ttl,
        max_uses,
    };
    let (_, token) = app.accounts.create_invite(invite).await.map_err(internal)?;
    Ok(Protobuf(CreatedShareLink {
        url: join_url(&app.auth.public_url, token.as_str()),
    }))
}

pub async fn share_links(
    State(app): State<AppState>,
    AuthUser(user): AuthUser,
    Path(doc): Path<String>,
) -> Result<Protobuf<ShareLinks>, StatusCode> {
    let doc = parse_doc(&doc)?;
    // Never claims: GET has no side effects. A list nobody has synced yet
    // simply has no links.
    if !app
        .accounts
        .member_or_unclaimed(user.id, doc)
        .await
        .map_err(internal)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let links = app.accounts.share_links(doc).await.map_err(internal)?;
    Ok(Protobuf(ShareLinks {
        links: links
            .into_iter()
            .map(|l| ShareLink {
                id: l.id.0,
                created_by_user_id: l.created_by.0,
                created_by_name: l.created_by_name,
                created_at: l.created_at,
                expires_at: l.expires_at,
                max_uses: l.max_uses.try_into().unwrap_or(u32::MAX),
                uses: l.uses.try_into().unwrap_or(u32::MAX),
            })
            .collect(),
    }))
}

pub async fn revoke_share_link(
    State(app): State<AppState>,
    AuthUser(user): AuthUser,
    Path((doc, id)): Path<(String, i64)>,
) -> Result<StatusCode, StatusCode> {
    let doc = parse_doc(&doc)?;
    // An unclaimed list has no links, so this still 404s for non-members.
    if !app
        .accounts
        .member_or_unclaimed(user.id, doc)
        .await
        .map_err(internal)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let revoked = app
        .accounts
        .revoke_share_link(doc, InviteId(id))
        .await
        .map_err(internal)?;
    if revoked {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn parse_doc(doc: &str) -> Result<DocId, StatusCode> {
    doc.parse().map_err(|_| StatusCode::NOT_FOUND)
}

fn internal(err: impl Display) -> StatusCode {
    tracing::error!("api: {err}");
    StatusCode::INTERNAL_SERVER_ERROR
}
