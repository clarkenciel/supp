//! One WebSocket connection: handshake, then multiplexed sync of every
//! document the client subscribes to.

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use automerge::sync;
use axum::{
    extract::{
        State,
        ws::{Message as WsMessage, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use axum_extra::extract::CookieJar;
use prost::Message as _;
use supp_shared::{
    DocId, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
    proto::sync::v1::{
        ClientMessage, Denied, Error, Left, ServerMessage, SessionEnded, SyncPayload,
        UpgradeRequired, Welcome, client_message, server_message,
    },
};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    AppState,
    accounts::{Access, Token, User},
    auth::{AuthUser, session::SESSION_COOKIE},
    docs::DocHandle,
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// How often an open connection re-checks its session, so logouts and
/// disabled accounts take effect without waiting for a reconnect.
const REVALIDATE_EVERY: Duration = Duration::from_secs(5 * 60);

/// Requires a session: an unauthenticated upgrade is refused with 401.
pub async fn handler(
    ws: WebSocketUpgrade,
    State(app): State<AppState>,
    AuthUser(user): AuthUser,
    jar: CookieJar,
) -> Response {
    let token = jar
        .get(SESSION_COOKIE)
        .map(|c| Token::from(c.value().to_string()))
        .expect("AuthUser found the session cookie");
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run(app, user, token, socket).await {
            tracing::debug!("connection closed: {err:#}");
        }
    })
}

struct Subscription {
    doc: Arc<DocHandle>,
    state: sync::State,
    _watcher: AbortOnDrop,
}

struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Connection {
    app: AppState,
    user: User,
    token: Token,
    subs: HashMap<DocId, Subscription>,
    /// Watcher tasks report which subscribed documents changed.
    changed_tx: mpsc::Sender<DocId>,
}

/// Replies to a client message, or the end of the connection.
enum Reply {
    Send(Vec<server_message::Msg>),
    SessionEnded,
}

async fn run(app: AppState, user: User, token: Token, mut socket: WebSocket) -> anyhow::Result<()> {
    if !handshake(&app, &user, &mut socket).await? {
        return Ok(());
    }

    let (changed_tx, mut changed_rx) = mpsc::channel::<DocId>(64);
    let mut conn = Connection {
        app,
        user,
        token,
        subs: HashMap::new(),
        changed_tx,
    };
    let mut revalidate = tokio::time::interval(REVALIDATE_EVERY);
    revalidate.tick().await;

    loop {
        tokio::select! {
            frame = socket.recv() => {
                let bytes = match frame {
                    None | Some(Ok(WsMessage::Close(_))) => return Ok(()),
                    Some(Err(err)) => return Err(err.into()),
                    Some(Ok(WsMessage::Binary(bytes))) => bytes,
                    Some(Ok(_)) => continue,
                };
                let msg = match ClientMessage::decode(bytes) {
                    Ok(msg) => msg,
                    Err(err) => {
                        send(&mut socket, error(format!("malformed message: {err}"))).await?;
                        continue;
                    }
                };
                match conn.handle(msg).await {
                    Reply::Send(replies) => {
                        for reply in replies {
                            send(&mut socket, reply).await?;
                        }
                    }
                    Reply::SessionEnded => return end_session(&mut socket).await,
                }
            }
            Some(id) = changed_rx.recv() => {
                if let Some(sub) = conn.subs.get_mut(&id)
                    && let Some(m) = sub.doc.generate(&mut sub.state).await
                {
                    send(&mut socket, sync_msg(id, m)).await?;
                }
            }
            _ = revalidate.tick() => {
                if !conn.session_valid().await {
                    return end_session(&mut socket).await;
                }
            }
        }
    }
}

/// Returns whether the client may proceed.
async fn handshake(app: &AppState, user: &User, socket: &mut WebSocket) -> anyhow::Result<bool> {
    let frame = tokio::time::timeout(HELLO_TIMEOUT, socket.recv())
        .await
        .context("no hello")?;
    let Some(Ok(WsMessage::Binary(bytes))) = frame else {
        bail!("connection ended before hello");
    };
    let hello = match ClientMessage::decode(bytes).ok().and_then(|m| m.msg) {
        Some(client_message::Msg::Hello(hello)) => hello,
        _ => {
            send(socket, error("expected hello".into())).await?;
            return Ok(false);
        }
    };
    if hello.protocol_version < MIN_PROTOCOL_VERSION {
        let upgrade = server_message::Msg::Upgrade(UpgradeRequired {
            min_protocol_version: MIN_PROTOCOL_VERSION,
        });
        send(socket, upgrade).await?;
        return Ok(false);
    }
    let docs = app.accounts.docs_for(user.id).await?;
    let welcome = server_message::Msg::Welcome(Welcome {
        protocol_version: PROTOCOL_VERSION,
        doc_ids: docs.iter().map(|d| d.as_bytes().to_vec()).collect(),
    });
    send(socket, welcome).await?;
    Ok(true)
}

async fn end_session(socket: &mut WebSocket) -> anyhow::Result<()> {
    send(socket, server_message::Msg::SessionEnded(SessionEnded {})).await?;
    socket.send(WsMessage::Close(None)).await?;
    Ok(())
}

impl Connection {
    async fn session_valid(&self) -> bool {
        match self.app.accounts.session_user(&self.token).await {
            Ok(user) => user.is_some(),
            Err(err) => {
                // Don't drop users over a transient database error.
                tracing::error!("revalidating session: {err}");
                true
            }
        }
    }

    async fn handle(&mut self, msg: ClientMessage) -> Reply {
        use client_message::Msg;
        let replies = match msg.msg {
            Some(Msg::Subscribe(sub)) => {
                let Ok(id) = DocId::from_bytes(&sub.doc_id) else {
                    return Reply::Send(vec![error("invalid doc id".into())]);
                };
                if !self.session_valid().await {
                    return Reply::SessionEnded;
                }
                self.subscribe(id).await
            }
            Some(Msg::Sync(payload)) => self.sync(payload).await,
            Some(Msg::Unsubscribe(unsub)) => {
                if let Ok(id) = DocId::from_bytes(&unsub.doc_id) {
                    self.subs.remove(&id);
                }
                vec![]
            }
            Some(Msg::Leave(leave)) => {
                let Ok(id) = DocId::from_bytes(&leave.doc_id) else {
                    return Reply::Send(vec![error("invalid doc id".into())]);
                };
                self.subs.remove(&id);
                match self.app.accounts.leave(self.user.id, id).await {
                    Ok(()) => vec![server_message::Msg::Left(Left {
                        doc_id: id.as_bytes().to_vec(),
                    })],
                    Err(err) => {
                        tracing::error!("leaving {id}: {err}");
                        vec![error("failed to leave list".into())]
                    }
                }
            }
            Some(Msg::Hello(_)) => vec![error("unexpected hello".into())],
            // Unknown variant from a newer client.
            None => vec![],
        };
        Reply::Send(replies)
    }

    async fn subscribe(&mut self, id: DocId) -> Vec<server_message::Msg> {
        match self.app.accounts.authorize(self.user.id, id).await {
            Ok(Access::Member | Access::Claimed) => {}
            Ok(Access::Denied) => {
                return vec![server_message::Msg::Denied(Denied {
                    doc_id: id.as_bytes().to_vec(),
                    reason: "not a member of this list".into(),
                })];
            }
            Err(err) => {
                tracing::error!("authorizing {id}: {err}");
                return vec![error("failed to check access".into())];
            }
        }
        let doc = match self.app.docs.get(id).await {
            Ok(doc) => doc,
            Err(err) => {
                tracing::error!("loading {id}: {err:#}");
                return vec![error("failed to load document".into())];
            }
        };
        let mut state = sync::State::new();
        let first = doc.generate(&mut state).await;
        let watcher = spawn_watcher(&doc, self.changed_tx.clone());
        self.subs.insert(
            id,
            Subscription {
                doc,
                state,
                _watcher: watcher,
            },
        );
        first.map(|m| sync_msg(id, m)).into_iter().collect()
    }

    async fn sync(&mut self, payload: SyncPayload) -> Vec<server_message::Msg> {
        let Ok(id) = DocId::from_bytes(&payload.doc_id) else {
            return vec![error("invalid doc id".into())];
        };
        let Some(sub) = self.subs.get_mut(&id) else {
            return vec![error(format!("not subscribed to {id}"))];
        };
        let message = match sync::Message::decode(&payload.data) {
            Ok(m) => m,
            Err(err) => return vec![error(format!("malformed sync message: {err}"))],
        };
        match sub.doc.receive(&mut sub.state, message).await {
            Ok(reply) => reply.map(|m| sync_msg(id, m)).into_iter().collect(),
            Err(err) => {
                tracing::warn!("sync {id}: {err:#}");
                vec![error("sync failed".into())]
            }
        }
    }
}

fn spawn_watcher(doc: &DocHandle, changed_tx: mpsc::Sender<DocId>) -> AbortOnDrop {
    let id = doc.id();
    let mut heads = doc.watch_heads();
    AbortOnDrop(tokio::spawn(async move {
        while heads.changed().await.is_ok() {
            if changed_tx.send(id).await.is_err() {
                break;
            }
        }
    }))
}

fn sync_msg(id: DocId, message: sync::Message) -> server_message::Msg {
    server_message::Msg::Sync(SyncPayload {
        doc_id: id.as_bytes().to_vec(),
        data: message.encode(),
    })
}

fn error(message: String) -> server_message::Msg {
    server_message::Msg::Error(Error { message })
}

async fn send(socket: &mut WebSocket, msg: server_message::Msg) -> anyhow::Result<()> {
    let bytes = ServerMessage { msg: Some(msg) }.encode_to_vec();
    socket.send(WsMessage::Binary(bytes.into())).await?;
    Ok(())
}
