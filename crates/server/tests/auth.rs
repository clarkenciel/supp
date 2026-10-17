//! Login, invite and session flows over HTTP, using the dev identity provider.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use prost::Message as _;
use supp_server::{
    AppState, Docs, Store,
    accounts::{Accounts, NewInvite, Token, UserId},
    app,
    auth::{AuthConfig, DevProvider},
};
use supp_shared::{
    DocId,
    proto::api::v1::{CreateShareLink, CreatedShareLink, Me, ShareLinks},
};
use tower::ServiceExt;

struct TestApp {
    _dir: tempfile::TempDir,
    router: Router,
    accounts: Accounts,
}

/// A browser: remembers cookies across requests.
#[derive(Default)]
struct Browser {
    cookies: HashMap<String, String>,
}

impl TestApp {
    async fn new() -> Self {
        Self::build(tempfile::tempdir().unwrap(), None).await
    }

    /// Also serves the web client from `dist` (inside `dir`).
    async fn with_static_dir(dir: tempfile::TempDir, dist: PathBuf) -> Self {
        Self::build(dir, Some(dist)).await
    }

    async fn build(dir: tempfile::TempDir, static_dir: Option<PathBuf>) -> Self {
        let store = Store::open(&dir.path().join("supp.db")).await.unwrap();
        let docs = Arc::new(Docs::new(store.docs(), 100));
        let auth = AuthConfig::new(Arc::new(DevProvider), "http://supp.test");
        let mut state = AppState::new(&store, docs, auth).await.unwrap();
        state.static_dir = static_dir;
        Self {
            _dir: dir,
            router: app(state),
            accounts: store.accounts(),
        }
    }

    async fn invite(&self, doc_id: Option<DocId>) -> Token {
        let invite = NewInvite {
            doc_id,
            created_by: None,
            note: String::new(),
            ttl: Duration::from_secs(3600),
            max_uses: 1,
        };
        self.accounts.create_invite(invite).await.unwrap().1
    }
}

impl Browser {
    async fn request(&mut self, app: &TestApp, method: Method, uri: &str) -> Response<Body> {
        self.send(app, method, uri, None).await
    }

    /// Sends a request with an optional protobuf body.
    async fn send(
        &mut self,
        app: &TestApp,
        method: Method,
        uri: &str,
        body: Option<Vec<u8>>,
    ) -> Response<Body> {
        let cookie = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie);
        if body.is_some() {
            request = request.header(header::CONTENT_TYPE, "application/x-protobuf");
        }
        let request = request
            .body(body.map_or_else(Body::empty, Body::from))
            .unwrap();
        let response = app.router.clone().oneshot(request).await.unwrap();
        for set in response.headers().get_all(header::SET_COOKIE) {
            let set = set.to_str().unwrap();
            let pair = set.split(';').next().unwrap();
            let (name, value) = pair.split_once('=').unwrap();
            let removed = set.contains("Max-Age=0") || value.is_empty();
            if removed {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_string(), value.to_string());
            }
        }
        response
    }

    async fn get(&mut self, app: &TestApp, uri: &str) -> Response<Body> {
        self.request(app, Method::GET, uri).await
    }

    /// Follows the dev provider's flow from a login or join URL, signing in
    /// as `name`. Returns the callback response.
    async fn sign_in(&mut self, app: &TestApp, start: &str, name: &str) -> Response<Body> {
        let response = self.get(app, start).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let to = location(&response);
        let state = to
            .strip_prefix("/auth/dev?state=")
            .expect("dev provider redirect");
        self.get(app, &format!("/auth/callback?state={state}&code={name}"))
            .await
    }

    async fn me(&mut self, app: &TestApp) -> Option<Me> {
        let response = self.get(app, "/api/me").await;
        match response.status() {
            StatusCode::OK => {
                let body = response.into_body().collect().await.unwrap().to_bytes();
                Some(Me::decode(body).unwrap())
            }
            StatusCode::UNAUTHORIZED => None,
            status => panic!("unexpected status {status}"),
        }
    }
}

fn location(response: &Response<Body>) -> String {
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn invite_sign_up_then_plain_sign_in() {
    let app = TestApp::new().await;
    let token = app.invite(None).await;

    let mut browser = Browser::default();
    assert!(browser.me(&app).await.is_none());
    let response = browser
        .sign_in(&app, &format!("/join/{}", token.as_str()), "alice")
        .await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(location(&response), "/");
    let me = browser.me(&app).await.unwrap();
    assert_eq!(me.name, "alice");
    assert!(
        !browser.cookies.contains_key("supp_login"),
        "login cookie is cleared"
    );

    // A new device: no invite needed for an existing account.
    let mut other = Browser::default();
    let response = other.sign_in(&app, "/auth/login", "alice").await;
    assert_eq!(location(&response), "/");
    assert_eq!(other.me(&app).await.unwrap().user_id, me.user_id);
}

#[tokio::test]
async fn sign_up_without_invite_is_refused() {
    let app = TestApp::new().await;
    let mut browser = Browser::default();
    let response = browser.sign_in(&app, "/auth/login", "mallory").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(browser.me(&app).await.is_none());
}

#[tokio::test]
async fn share_link_lands_on_the_list() {
    let app = TestApp::new().await;
    let doc = DocId::new();
    let token = app.invite(Some(doc)).await;
    let mut browser = Browser::default();
    let response = browser
        .sign_in(&app, &format!("/join/{}", token.as_str()), "bob")
        .await;
    assert_eq!(location(&response), format!("/lists/{doc}?joined=1"));
}

#[tokio::test]
async fn callback_rejects_wrong_state_and_missing_cookie() {
    let app = TestApp::new().await;
    let token = app.invite(None).await;

    let mut browser = Browser::default();
    browser
        .get(&app, &format!("/join/{}", token.as_str()))
        .await;
    let response = browser
        .get(&app, "/auth/callback?state=forged&code=eve")
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let mut fresh = Browser::default();
    let response = fresh.get(&app, "/auth/callback?state=x&code=eve").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Neither attempt used up the invite.
    let mut real = Browser::default();
    let response = real
        .sign_in(&app, &format!("/join/{}", token.as_str()), "carol")
        .await;
    assert_eq!(location(&response), "/");
}

#[tokio::test]
async fn tampered_login_cookie_is_rejected() {
    let app = TestApp::new().await;
    let mut browser = Browser::default();
    browser.get(&app, "/auth/login").await;
    browser
        .cookies
        .insert("supp_login".into(), "not-encrypted".into());
    let response = browser.get(&app, "/auth/callback?state=x&code=eve").await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn logout_ends_the_session() {
    let app = TestApp::new().await;
    let token = app.invite(None).await;
    let mut browser = Browser::default();
    browser
        .sign_in(&app, &format!("/join/{}", token.as_str()), "dave")
        .await;
    let session = browser.cookies["supp_session"].clone();

    let response = browser.request(&app, Method::POST, "/auth/logout").await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(browser.me(&app).await.is_none());

    // The old cookie value is dead server-side too.
    browser.cookies.insert("supp_session".into(), session);
    assert!(browser.me(&app).await.is_none());
}

#[tokio::test]
async fn disabled_users_lose_their_session() {
    let app = TestApp::new().await;
    let token = app.invite(None).await;
    let mut browser = Browser::default();
    browser
        .sign_in(&app, &format!("/join/{}", token.as_str()), "erin")
        .await;
    let me = browser.me(&app).await.unwrap();
    app.accounts.disable_user(UserId(me.user_id)).await.unwrap();
    assert!(browser.me(&app).await.is_none());
    let response = browser.sign_in(&app, "/auth/login", "erin").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn serves_the_web_client_with_spa_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let dist = dir.path().join("dist");
    std::fs::create_dir(&dist).unwrap();
    std::fs::write(dist.join("index.html"), "<html>app</html>").unwrap();
    std::fs::write(dist.join("app.js"), "console.log(1)").unwrap();

    let app = TestApp::with_static_dir(dir, dist).await;

    let mut browser = Browser::default();
    let body = |r: Response<Body>| async { r.into_body().collect().await.unwrap().to_bytes() };
    assert_eq!(body(browser.get(&app, "/").await).await, "<html>app</html>");
    assert_eq!(
        body(browser.get(&app, "/app.js").await).await,
        "console.log(1)"
    );
    let deep = browser.get(&app, &format!("/lists/{}", DocId::new())).await;
    assert_eq!(deep.status(), StatusCode::OK);
    assert_eq!(body(deep).await, "<html>app</html>");
    // API routes still win over the fallback.
    assert_eq!(
        browser.get(&app, "/api/me").await.status(),
        StatusCode::UNAUTHORIZED
    );
    // A missing file is not a client route: no index.html in its place.
    assert_eq!(
        browser
            .get(&app, "/supp-web-fedcba9876543210.js")
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn caches_hashed_assets_forever_and_revalidates_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let dist = dir.path().join("dist");
    std::fs::create_dir_all(dist.join("icons")).unwrap();
    for file in [
        "index.html",
        "sw.js",
        "manifest.webmanifest",
        "icons/icon-192.png",
        "supp-web-0123456789abcdef.js",
        "supp-web-0123456789abcdef_bg.wasm",
        "style-0123456789abcdef.css",
    ] {
        std::fs::write(dist.join(file), file).unwrap();
    }
    let app = TestApp::with_static_dir(dir, dist).await;

    let mut browser = Browser::default();
    let cache_control = |r: &Response<Body>| {
        assert_eq!(r.status(), StatusCode::OK);
        r.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .to_string()
    };
    let immutable = "public, max-age=31536000, immutable";
    for path in [
        "/supp-web-0123456789abcdef.js",
        "/supp-web-0123456789abcdef_bg.wasm",
        "/style-0123456789abcdef.css",
    ] {
        assert_eq!(
            cache_control(&browser.get(&app, path).await),
            immutable,
            "{path}"
        );
    }
    let deep = format!("/lists/{}", DocId::new());
    for path in [
        "/",
        "/index.html",
        &deep,
        "/sw.js",
        "/manifest.webmanifest",
        "/icons/icon-192.png",
    ] {
        assert_eq!(
            cache_control(&browser.get(&app, path).await),
            "no-cache",
            "{path}"
        );
    }
}

// Share links over the API.

async fn body(response: Response<Body>) -> axum::body::Bytes {
    response.into_body().collect().await.unwrap().to_bytes()
}

impl TestApp {
    /// A browser signed up as `name` with an account invite.
    async fn user(&self, name: &str) -> (Browser, UserId) {
        let token = self.invite(None).await;
        let mut browser = Browser::default();
        browser
            .sign_in(self, &format!("/join/{}", token.as_str()), name)
            .await;
        let id = UserId(browser.me(self).await.unwrap().user_id);
        (browser, id)
    }
}

impl Browser {
    async fn create_link(
        &mut self,
        app: &TestApp,
        doc: DocId,
        request: CreateShareLink,
    ) -> Response<Body> {
        let uri = format!("/api/lists/{doc}/invites");
        self.send(app, Method::POST, &uri, Some(request.encode_to_vec()))
            .await
    }

    /// Creates a share link and returns its path (`/join/…`).
    async fn share(&mut self, app: &TestApp, doc: DocId, request: CreateShareLink) -> String {
        let response = self.create_link(app, doc, request).await;
        assert_eq!(response.status(), StatusCode::OK);
        let created = CreatedShareLink::decode(body(response).await).unwrap();
        created
            .url
            .strip_prefix("http://supp.test")
            .expect("built from the public URL")
            .to_string()
    }

    async fn links(&mut self, app: &TestApp, doc: DocId) -> Result<ShareLinks, StatusCode> {
        let response = self.get(app, &format!("/api/lists/{doc}/invites")).await;
        match response.status() {
            StatusCode::OK => Ok(ShareLinks::decode(body(response).await).unwrap()),
            status => Err(status),
        }
    }

    async fn revoke(&mut self, app: &TestApp, doc: DocId, id: i64) -> StatusCode {
        let uri = format!("/api/lists/{doc}/invites/{id}");
        self.request(app, Method::DELETE, &uri).await.status()
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::test]
async fn members_share_lists_with_new_users() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();

    let link = owner.share(&app, doc, CreateShareLink::default()).await;
    assert!(link.starts_with("/join/"));
    let links = owner.links(&app, doc).await.unwrap().links;
    assert_eq!(links.len(), 1);
    let info = &links[0];
    assert_eq!(
        (info.max_uses, info.uses, info.created_by_user_id),
        (1, 0, owner_id.0)
    );
    assert_eq!(info.created_by_name, "owner");
    let week = 7 * 24 * 60 * 60;
    assert!((info.expires_at - now() - week).abs() < 5, "7-day default");

    // Someone with no account signs up through it and lands on the list.
    let mut guest = Browser::default();
    let response = guest.sign_in(&app, &link, "guest").await;
    assert_eq!(location(&response), format!("/lists/{doc}?joined=1"));
    let guest_id = UserId(guest.me(&app).await.unwrap().user_id);
    assert_eq!(app.accounts.docs_for(guest_id).await.unwrap(), vec![doc]);

    // The guest is a member now, and sees the link used up.
    let links = guest.links(&app, doc).await.unwrap().links;
    assert_eq!((links[0].uses, links[0].max_uses), (1, 1));

    // Single use by default.
    let mut late = Browser::default();
    let response = late.sign_in(&app, &link, "late").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn share_links_need_a_session_and_membership() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let (mut stranger, _) = app.user("stranger").await;
    let doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();
    owner.share(&app, doc, CreateShareLink::default()).await;
    let id = owner.links(&app, doc).await.unwrap().links[0].id;

    let mut anonymous = Browser::default();
    let response = anonymous
        .create_link(&app, doc, CreateShareLink::default())
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        anonymous.links(&app, doc).await.unwrap_err(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        anonymous.revoke(&app, doc, id).await,
        StatusCode::UNAUTHORIZED
    );

    let response = stranger
        .create_link(&app, doc, CreateShareLink::default())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        stranger.links(&app, doc).await.unwrap_err(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(stranger.revoke(&app, doc, id).await, StatusCode::NOT_FOUND);

    // Leaving the list ends access too.
    app.accounts.leave(owner_id, doc).await.unwrap();
    let response = owner
        .create_link(&app, doc, CreateShareLink::default())
        .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        owner.links(&app, doc).await.unwrap_err(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(owner.revoke(&app, doc, id).await, StatusCode::NOT_FOUND);

    // Garbage list IDs are just missing lists.
    let response = owner.get(&app, "/api/lists/nope/invites").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sharing_a_list_not_synced_yet_claims_it() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let doc = DocId::new();
    // Looking doesn't claim it...
    assert!(owner.links(&app, doc).await.unwrap().links.is_empty());
    assert!(app.accounts.docs_for(owner_id).await.unwrap().is_empty());
    // ...sharing does, just as subscribing would.
    owner.share(&app, doc, CreateShareLink::default()).await;
    assert_eq!(app.accounts.docs_for(owner_id).await.unwrap(), vec![doc]);
}

#[tokio::test]
async fn existing_users_join_through_share_links() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let (_, friend_id) = app.user("friend").await;
    let doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();
    let link = owner.share(&app, doc, CreateShareLink::default()).await;

    // The friend's other device: an existing account signing in via the link.
    let mut laptop = Browser::default();
    let response = laptop.sign_in(&app, &link, "friend").await;
    assert_eq!(location(&response), format!("/lists/{doc}?joined=1"));
    assert_eq!(app.accounts.docs_for(friend_id).await.unwrap(), vec![doc]);
}

#[tokio::test]
async fn rejoining_a_left_list_revives_the_membership() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let (mut friend, friend_id) = app.user("friend").await;
    let doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();
    let link = owner.share(&app, doc, CreateShareLink::default()).await;
    friend.sign_in(&app, &link, "friend").await;
    app.accounts.leave(friend_id, doc).await.unwrap();
    assert!(app.accounts.docs_for(friend_id).await.unwrap().is_empty());

    // The landing URL says the list was joined, so a device that deleted
    // it offline knows to drop its pending leave instead of sending it.
    let link = owner.share(&app, doc, CreateShareLink::default()).await;
    let response = friend.sign_in(&app, &link, "friend").await;
    assert_eq!(location(&response), format!("/lists/{doc}?joined=1"));
    assert_eq!(app.accounts.docs_for(friend_id).await.unwrap(), vec![doc]);
}

#[tokio::test]
async fn any_member_can_revoke_a_link() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let doc = DocId::new();
    let other_doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();
    app.accounts.authorize(owner_id, other_doc).await.unwrap();

    let uses = CreateShareLink {
        max_uses: 5,
        ..Default::default()
    };
    let first = owner.share(&app, doc, uses).await;
    let second = owner.share(&app, doc, uses).await;
    let mut member = Browser::default();
    member.sign_in(&app, &first, "member").await;
    let links = member.links(&app, doc).await.unwrap().links;
    assert_eq!(links.len(), 2);
    let (first_id, second_id) = (links[0].id, links[1].id);

    // Only through the link's own list.
    assert_eq!(
        owner.revoke(&app, other_doc, second_id).await,
        StatusCode::NOT_FOUND
    );
    // A member who didn't create it may revoke it.
    assert_eq!(
        member.revoke(&app, doc, second_id).await,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        owner.revoke(&app, doc, second_id).await,
        StatusCode::NOT_FOUND,
        "already revoked"
    );
    let links = owner.links(&app, doc).await.unwrap().links;
    assert_eq!(links.iter().map(|l| l.id).collect::<Vec<_>>(), [first_id]);

    let mut late = Browser::default();
    let response = late.sign_in(&app, &second, "late").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn share_link_limits_are_enforced() {
    let app = TestApp::new().await;
    let (mut owner, owner_id) = app.user("owner").await;
    let doc = DocId::new();
    app.accounts.authorize(owner_id, doc).await.unwrap();

    for bad in [
        CreateShareLink {
            max_uses: 101,
            ..Default::default()
        },
        CreateShareLink {
            expires_in_secs: 31 * 24 * 60 * 60,
            ..Default::default()
        },
    ] {
        let response = owner.create_link(&app, doc, bad).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    let not_protobuf = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/lists/{doc}/invites"))
        .header(
            header::COOKIE,
            format!("supp_session={}", owner.cookies["supp_session"]),
        )
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("max_uses=1"))
        .unwrap();
    let response = app.router.clone().oneshot(not_protobuf).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert!(owner.links(&app, doc).await.unwrap().links.is_empty());

    let request = CreateShareLink {
        max_uses: 2,
        expires_in_secs: 3600,
    };
    let link = owner.share(&app, doc, request).await;
    let info = &owner.links(&app, doc).await.unwrap().links[0];
    assert!((info.expires_at - now() - 3600).abs() < 5);
    for name in ["a", "b"] {
        let response = Browser::default().sign_in(&app, &link, name).await;
        assert_eq!(location(&response), format!("/lists/{doc}?joined=1"));
    }
    let response = Browser::default().sign_in(&app, &link, "c").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
