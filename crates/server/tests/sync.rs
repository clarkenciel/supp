//! End-to-end sync tests: real server, real WebSocket clients.

use std::{net::SocketAddr, path::Path, sync::Arc, time::Duration};

use automerge::{
    AutoCommit,
    sync::{self, SyncDoc},
};
use autosurgeon::{hydrate, reconcile};
use futures_util::{SinkExt, StreamExt};
use prost::Message as _;
use supp_server::{
    AppState, Docs, Store,
    accounts::{Accounts, Identity, NewInvite, Token, UserId},
    app,
    auth::{AuthConfig, DevProvider},
};
use supp_shared::{
    DocId, PROTOCOL_VERSION,
    proto::sync::v1::{
        ClientMessage, Hello, ServerMessage, Subscribe, SyncPayload, client_message, server_message,
    },
    schema::{Item, ListDoc},
};
use tokio::{net::TcpStream, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

const DEADLINE: Duration = Duration::from_secs(5);

struct Server {
    addr: SocketAddr,
    store: Store,
    accounts: Accounts,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl Server {
    async fn start(db: &Path, compact_after: usize) -> Self {
        let store = Store::open(db).await.unwrap();
        let docs = Arc::new(Docs::new(store.clone(), compact_after));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let auth = AuthConfig::new(Arc::new(DevProvider), &format!("http://{addr}"));
        let state = AppState::new(&store, docs, auth).await.unwrap();
        let (shutdown, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, app(state))
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await
                .unwrap();
        });
        Self {
            addr,
            accounts: store.accounts(),
            store,
            shutdown,
            task,
        }
    }

    /// Signs `name` up (or in) and returns a session token and user ID.
    async fn session(&self, name: &str) -> (Token, UserId) {
        let invite = NewInvite {
            doc_id: None,
            created_by: None,
            note: String::new(),
            ttl: Duration::from_secs(60),
            max_uses: 1,
        };
        let (_, invite) = self.accounts.create_invite(invite).await.unwrap();
        let identity = Identity {
            subject: format!("test:{name}"),
            email: format!("{name}@example.com"),
            name: name.into(),
        };
        let user = self
            .accounts
            .login(&identity, Some(&invite))
            .await
            .unwrap()
            .user;
        (
            self.accounts.create_session(user.id).await.unwrap(),
            user.id,
        )
    }

    async fn stop(self) {
        let _ = self.shutdown.send(());
        self.task.await.unwrap();
        self.store.close().await;
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn try_connect(addr: SocketAddr, session: Option<&Token>) -> Result<Ws, String> {
    let mut request = format!("ws://{addr}/sync").into_client_request().unwrap();
    if let Some(token) = session {
        let cookie = HeaderValue::from_str(&format!("supp_session={}", token.as_str())).unwrap();
        request.headers_mut().insert("cookie", cookie);
    }
    connect_async(request)
        .await
        .map(|(ws, _)| ws)
        .map_err(|e| e.to_string())
}

async fn connect_raw(addr: SocketAddr, session: &Token) -> Ws {
    try_connect(addr, Some(session)).await.unwrap()
}

async fn send(ws: &mut Ws, msg: client_message::Msg) {
    let bytes = ClientMessage { msg: Some(msg) }.encode_to_vec();
    ws.send(Message::Binary(bytes.into())).await.unwrap();
}

async fn recv(ws: &mut Ws) -> Option<server_message::Msg> {
    loop {
        match ws.next().await? {
            Ok(Message::Binary(bytes)) => return ServerMessage::decode(bytes).unwrap().msg,
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => continue,
        }
    }
}

fn hello(version: u32) -> client_message::Msg {
    client_message::Msg::Hello(Hello {
        protocol_version: version,
        client_build: "test".into(),
    })
}

/// A peer syncing a single document.
struct Client {
    ws: Ws,
    id: DocId,
    doc: AutoCommit,
    state: sync::State,
}

impl Client {
    async fn connect(addr: SocketAddr, session: &Token, id: DocId, doc: AutoCommit) -> Self {
        let mut ws = connect_raw(addr, session).await;
        send(&mut ws, hello(PROTOCOL_VERSION)).await;
        assert!(matches!(
            recv(&mut ws).await,
            Some(server_message::Msg::Welcome(_))
        ));
        send(
            &mut ws,
            client_message::Msg::Subscribe(Subscribe {
                doc_id: id.as_bytes().to_vec(),
            }),
        )
        .await;
        let mut client = Self {
            ws,
            id,
            doc,
            state: sync::State::new(),
        };
        client.flush().await;
        client
    }

    fn list(&self) -> ListDoc {
        hydrate(&self.doc).unwrap()
    }

    async fn edit(&mut self, f: impl FnOnce(&mut ListDoc)) {
        let mut list = self.list();
        f(&mut list);
        reconcile(&mut self.doc, &list).unwrap();
        self.flush().await;
        self.settle().await;
    }

    /// Exchanges messages until the server has acknowledged everything we have.
    async fn settle(&mut self) {
        self.until_state(|c| {
            let mut ours = c.doc.get_heads();
            let mut shared = c.state.shared_heads.clone();
            ours.sort();
            shared.sort();
            ours == shared
        })
        .await;
    }

    /// Sends a sync message if there is anything to send.
    async fn flush(&mut self) {
        if let Some(m) = self.doc.sync().generate_sync_message(&mut self.state) {
            let payload = SyncPayload {
                doc_id: self.id.as_bytes().to_vec(),
                data: m.encode(),
            };
            send(&mut self.ws, client_message::Msg::Sync(payload)).await;
        }
    }

    /// Processes incoming messages until `done` holds for the document.
    async fn until(&mut self, done: impl Fn(&ListDoc) -> bool) {
        self.until_state(|c| done(&c.list())).await;
    }

    async fn until_state(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        tokio::time::timeout(DEADLINE, async {
            while !done(self) {
                match recv(&mut self.ws).await {
                    Some(server_message::Msg::Sync(p)) => {
                        assert_eq!(p.doc_id, self.id.as_bytes());
                        let m = sync::Message::decode(&p.data).unwrap();
                        self.doc
                            .sync()
                            .receive_sync_message(&mut self.state, m)
                            .unwrap();
                        self.flush().await;
                    }
                    other => panic!("unexpected message: {other:?}"),
                }
            }
        })
        .await
        .expect("condition not reached before deadline");
    }
}

fn item(text: &str, position: &str) -> Item {
    Item {
        text: text.into(),
        done: false,
        position: position.into(),
    }
}

fn titled(title: &'static str) -> impl Fn(&ListDoc) -> bool {
    move |l| l.title == title
}

#[tokio::test]
async fn two_clients_converge() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, _) = server.session("alice").await;
    let id = DocId::new();

    let mut a = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    a.edit(|l| {
        l.title = "groceries".into();
        l.items.insert("milk".into(), item("milk", "a0"));
    })
    .await;

    let mut b = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    b.until(titled("groceries")).await;

    // Concurrent edits from both sides; each must reach the other.
    a.edit(|l| {
        l.items.insert("eggs".into(), item("eggs", "a1"));
    })
    .await;
    b.edit(|l| {
        l.items.get_mut("milk").unwrap().done = true;
        l.items.insert("bread".into(), item("bread", "a2"));
    })
    .await;

    let converged = |l: &ListDoc| l.items.len() == 3 && l.items["milk"].done;
    a.until(converged).await;
    b.until(converged).await;
    assert_eq!(a.list(), b.list());

    server.stop().await;
}

#[tokio::test]
async fn persists_across_restart_and_compacts() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("supp.db");
    let id = DocId::new();

    let server = Server::start(&db, 3).await;
    let (session, _) = server.session("alice").await;
    let mut a = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    for i in 0..7 {
        a.edit(|l| {
            l.title = "chores".into();
            l.items
                .insert(format!("{i}"), item(&format!("task {i}"), "a0"));
        })
        .await;
    }
    // A second client seeing every item proves the server applied them.
    let mut b = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    b.until(|l| l.items.len() == 7).await;
    drop((a, b));
    server.stop().await;

    let server = Server::start(&db, 3).await;
    let (session, _) = server.session("alice").await;
    assert!(server.store.change_rows(id).await.unwrap() < 3);
    let mut c = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    c.until(|l| l.title == "chores" && l.items.len() == 7).await;
    server.stop().await;
}

#[tokio::test]
async fn offline_edits_sync_on_reconnect() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, _) = server.session("alice").await;
    let id = DocId::new();

    let mut a = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    a.edit(|l| l.title = "trip".into()).await;
    let mut b = Client::connect(server.addr, &session, id, AutoCommit::new()).await;
    b.until(titled("trip")).await;

    // B goes offline and edits locally; sync state resets on reconnect.
    let mut offline = b.doc.fork();
    drop(b);
    let mut list: ListDoc = hydrate(&offline).unwrap();
    list.items.insert("tent".into(), item("tent", "a0"));
    reconcile(&mut offline, &list).unwrap();

    let mut b = Client::connect(server.addr, &session, id, offline).await;
    b.settle().await;
    a.until(|l| l.items.contains_key("tent")).await;

    server.stop().await;
}

#[tokio::test]
async fn rejects_old_protocol_version() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, _) = server.session("alice").await;

    let mut ws = connect_raw(server.addr, &session).await;
    send(&mut ws, hello(0)).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(server_message::Msg::Upgrade(_))
    ));
    assert!(recv(&mut ws).await.is_none());

    server.stop().await;
}

#[tokio::test]
async fn requires_hello_first() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, _) = server.session("alice").await;

    let mut ws = connect_raw(server.addr, &session).await;
    let subscribe = client_message::Msg::Subscribe(Subscribe {
        doc_id: DocId::new().as_bytes().to_vec(),
    });
    send(&mut ws, subscribe).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(server_message::Msg::Error(_))
    ));
    assert!(recv(&mut ws).await.is_none());

    server.stop().await;
}

#[tokio::test]
async fn sync_requires_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, _) = server.session("alice").await;

    let mut ws = connect_raw(server.addr, &session).await;
    send(&mut ws, hello(PROTOCOL_VERSION)).await;
    recv(&mut ws).await;
    let mut doc = AutoCommit::new();
    let m = doc
        .sync()
        .generate_sync_message(&mut sync::State::new())
        .unwrap();
    let payload = SyncPayload {
        doc_id: DocId::new().as_bytes().to_vec(),
        data: m.encode(),
    };
    send(&mut ws, client_message::Msg::Sync(payload)).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(server_message::Msg::Error(_))
    ));

    server.stop().await;
}

async fn welcome(ws: &mut Ws) -> Vec<DocId> {
    send(ws, hello(PROTOCOL_VERSION)).await;
    match recv(ws).await {
        Some(server_message::Msg::Welcome(w)) => w
            .doc_ids
            .iter()
            .map(|b| DocId::from_bytes(b).unwrap())
            .collect(),
        other => panic!("expected welcome, got {other:?}"),
    }
}

fn subscribe(id: DocId) -> client_message::Msg {
    client_message::Msg::Subscribe(Subscribe {
        doc_id: id.as_bytes().to_vec(),
    })
}

#[tokio::test]
async fn refuses_unauthenticated_connections() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;

    let err = try_connect(server.addr, None).await.unwrap_err();
    assert!(err.contains("401"), "{err}");
    let err = try_connect(server.addr, Some(&Token::generate()))
        .await
        .unwrap_err();
    assert!(err.contains("401"), "{err}");

    server.stop().await;
}

#[tokio::test]
async fn lists_belong_to_whoever_syncs_them_first() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (alice, _) = server.session("alice").await;
    let (bob, _) = server.session("bob").await;
    let id = DocId::new();

    let mut a = Client::connect(server.addr, &alice, id, AutoCommit::new()).await;
    a.edit(|l| l.title = "alice's".into()).await;

    let mut ws = connect_raw(server.addr, &bob).await;
    assert!(welcome(&mut ws).await.is_empty());
    send(&mut ws, subscribe(id)).await;
    match recv(&mut ws).await {
        Some(server_message::Msg::Denied(d)) => assert_eq!(d.doc_id, id.as_bytes()),
        other => panic!("expected denied, got {other:?}"),
    }

    // Alice's other devices learn about the list from the welcome.
    let mut ws = connect_raw(server.addr, &alice).await;
    assert_eq!(welcome(&mut ws).await, vec![id]);

    server.stop().await;
}

#[tokio::test]
async fn ended_sessions_are_told_and_disconnected() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(&dir.path().join("supp.db"), 100).await;
    let (session, user) = server.session("alice").await;

    let mut ws = connect_raw(server.addr, &session).await;
    welcome(&mut ws).await;
    server.accounts.disable_user(user).await.unwrap();
    send(&mut ws, subscribe(DocId::new())).await;
    assert!(matches!(
        recv(&mut ws).await,
        Some(server_message::Msg::SessionEnded(_))
    ));
    assert!(recv(&mut ws).await.is_none());

    server.stop().await;
}
