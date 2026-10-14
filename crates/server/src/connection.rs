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
use prost::Message as _;
use supp_shared::{
    DocId, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
    proto::sync::v1::{
        ClientMessage, Error, ServerMessage, SyncPayload, UpgradeRequired, Welcome, client_message,
        server_message,
    },
};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{AppState, docs::DocHandle};

const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn handler(ws: WebSocketUpgrade, State(app): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| async move {
        if let Err(err) = run(app, socket).await {
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

async fn run(app: AppState, mut socket: WebSocket) -> anyhow::Result<()> {
    if !handshake(&mut socket).await? {
        return Ok(());
    }

    let mut subs: HashMap<DocId, Subscription> = HashMap::new();
    // Watcher tasks report which subscribed documents changed.
    let (changed_tx, mut changed_rx) = mpsc::channel::<DocId>(64);

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
                for reply in handle(&app, &mut subs, &changed_tx, msg).await {
                    send(&mut socket, reply).await?;
                }
            }
            Some(id) = changed_rx.recv() => {
                if let Some(sub) = subs.get_mut(&id)
                    && let Some(m) = sub.doc.generate(&mut sub.state).await
                {
                    send(&mut socket, sync_msg(id, m)).await?;
                }
            }
        }
    }
}

/// Returns whether the client may proceed.
async fn handshake(socket: &mut WebSocket) -> anyhow::Result<bool> {
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
    let welcome = server_message::Msg::Welcome(Welcome {
        protocol_version: PROTOCOL_VERSION,
    });
    send(socket, welcome).await?;
    Ok(true)
}

async fn handle(
    app: &AppState,
    subs: &mut HashMap<DocId, Subscription>,
    changed_tx: &mpsc::Sender<DocId>,
    msg: ClientMessage,
) -> Vec<server_message::Msg> {
    use client_message::Msg;
    match msg.msg {
        Some(Msg::Subscribe(sub)) => {
            let Ok(id) = DocId::from_bytes(&sub.doc_id) else {
                return vec![error("invalid doc id".into())];
            };
            // TODO(milestone 3): check list membership.
            let doc = match app.docs.get(id).await {
                Ok(doc) => doc,
                Err(err) => {
                    tracing::error!("loading {id}: {err:#}");
                    return vec![error("failed to load document".into())];
                }
            };
            let mut state = sync::State::new();
            let first = doc.generate(&mut state).await;
            let watcher = spawn_watcher(&doc, changed_tx.clone());
            subs.insert(
                id,
                Subscription {
                    doc,
                    state,
                    _watcher: watcher,
                },
            );
            first.map(|m| sync_msg(id, m)).into_iter().collect()
        }
        Some(Msg::Sync(payload)) => {
            let Ok(id) = DocId::from_bytes(&payload.doc_id) else {
                return vec![error("invalid doc id".into())];
            };
            let Some(sub) = subs.get_mut(&id) else {
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
        Some(Msg::Unsubscribe(unsub)) => {
            if let Ok(id) = DocId::from_bytes(&unsub.doc_id) {
                subs.remove(&id);
            }
            vec![]
        }
        Some(Msg::Hello(_)) => vec![error("unexpected hello".into())],
        // Unknown variant from a newer client.
        None => vec![],
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
