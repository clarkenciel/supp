//! Login, invite and session flows over HTTP, using the dev identity provider.

use std::{collections::HashMap, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Method, Request, Response, StatusCode, header},
};
use http_body_util::BodyExt;
use prost::Message as _;
use supp_server::{
    AppState, Docs, Store,
    accounts::{Accounts, NewInvite, Token},
    app,
    auth::{AuthConfig, DevProvider},
};
use supp_shared::{DocId, proto::api::v1::Me};
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
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("supp.db")).await.unwrap();
        let docs = Arc::new(Docs::new(store.clone(), 100));
        let auth = AuthConfig::new(Arc::new(DevProvider), "http://supp.test");
        let state = AppState::new(&store, docs, auth).await.unwrap();
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
        let cookie = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, cookie)
            .body(Body::empty())
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
    assert_eq!(location(&response), format!("/lists/{doc}"));
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
    app.accounts
        .disable_user(supp_server::accounts::UserId(me.user_id))
        .await
        .unwrap();
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

    let store = Store::open(&dir.path().join("supp.db")).await.unwrap();
    let docs = Arc::new(Docs::new(store.clone(), 100));
    let auth = AuthConfig::new(Arc::new(DevProvider), "http://supp.test");
    let mut state = AppState::new(&store, docs, auth).await.unwrap();
    state.static_dir = Some(dist);
    let app = TestApp {
        _dir: dir,
        router: supp_server::app(state),
        accounts: store.accounts(),
    };

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
}
