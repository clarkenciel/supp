//! Keeps local lists in sync with the server while signed in.
//!
//! One WebSocket carries every list. After the handshake the client learns
//! the account's lists from `Welcome`, adds any it lacks, and subscribes to
//! every local list. Lists made while signed out upload this way on first
//! sign-in. Sync state is per connection and starts fresh on reconnect.

use std::{collections::HashMap, rc::Rc, time::Duration};

use automerge::sync;
use futures::{SinkExt, StreamExt, channel::mpsc, channel::oneshot, future::FutureExt, select};
use gloo_net::websocket::{Message, futures::WebSocket};
use gloo_timers::future::sleep;
use leptos::{logging, prelude::*, task::spawn_local};
use prost::Message as _;
use supp_shared::{
    DocId, PROTOCOL_VERSION,
    proto::sync::v1::{
        ClientMessage, Hello, Subscribe, SyncPayload, client_message, server_message,
    },
};

use crate::{
    repo::Repo,
    session::{Session, fetch_me},
};

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncStatus {
    /// Not signed in: nothing to sync.
    Idle,
    Connecting,
    Online,
    /// Disconnected; retrying. Edits are kept locally meanwhile.
    Offline,
    /// The server no longer accepts this version of the app.
    UpgradeRequired,
}

/// Runs the sync loop whenever the session is `SignedIn`, and stops it
/// otherwise.
pub fn run_while_signed_in(repo: Repo, session: RwSignal<Session>, status: RwSignal<SyncStatus>) {
    // Dropping the sender cancels the running loop.
    let running = StoredValue::new_local(None::<oneshot::Sender<()>>);
    Effect::new(move |_| {
        let signed_in = matches!(session.get(), Session::SignedIn(_));
        let is_running = running.with_value(|r| r.is_some());
        if signed_in && !is_running {
            let (stop, stopped) = oneshot::channel();
            running.set_value(Some(stop));
            spawn_local(run(repo, session, status, stopped));
        } else if !signed_in && is_running {
            running.set_value(None);
            status.set(SyncStatus::Idle);
        }
    });
}

enum Outcome {
    /// Connection lost (or never made); `welcomed` if it got past the
    /// handshake.
    Closed {
        welcomed: bool,
    },
    SessionEnded,
    UpgradeRequired,
}

async fn run(
    repo: Repo,
    session: RwSignal<Session>,
    status: RwSignal<SyncStatus>,
    mut stopped: oneshot::Receiver<()>,
) {
    let mut backoff = MIN_BACKOFF;
    loop {
        status.set(SyncStatus::Connecting);
        let outcome = select! {
            outcome = connect(repo, status).fuse() => outcome,
            _ = stopped => return,
        };
        match outcome {
            Outcome::SessionEnded => return expire(session),
            Outcome::UpgradeRequired => {
                status.set(SyncStatus::UpgradeRequired);
                return;
            }
            Outcome::Closed { welcomed } => {
                status.set(SyncStatus::Offline);
                if welcomed {
                    backoff = MIN_BACKOFF;
                } else if let Ok(None) = fetch_me().await {
                    // From JS a refused upgrade (401) looks like any other
                    // failed connection, so ask the server directly.
                    return expire(session);
                }
                // Jitter spreads reconnects when the server comes back.
                let jitter = js_sys::Math::random() * 0.5 + 0.75;
                select! {
                    _ = sleep(backoff.mul_f64(jitter)).fuse() => {}
                    _ = stopped => return,
                }
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

fn expire(session: RwSignal<Session>) {
    session.update(|s| {
        if let Session::SignedIn(me) = s {
            *s = Session::Expired(me.clone());
        }
    });
}

/// Clears the repo's change listener when the connection ends.
struct ListenerGuard(Repo);

impl Drop for ListenerGuard {
    fn drop(&mut self) {
        self.0.set_change_listener(None);
    }
}

type Sink = futures::stream::SplitSink<WebSocket, Message>;

async fn connect(repo: Repo, status: RwSignal<SyncStatus>) -> Outcome {
    let closed = Outcome::Closed { welcomed: false };
    let Ok(ws) = WebSocket::open(&sync_url()) else {
        return closed;
    };
    let (mut sink, stream) = ws.split();
    let mut stream = stream.fuse();
    let hello = client_message::Msg::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        client_build: env!("CARGO_PKG_VERSION").into(),
    });
    if send(&mut sink, hello).await.is_err() {
        return closed;
    }

    let (changed_tx, mut changed) = mpsc::unbounded::<DocId>();
    repo.set_change_listener(Some(Rc::new(move |id| {
        let _ = changed_tx.unbounded_send(id);
    })));
    let _guard = ListenerGuard(repo);

    let mut conn = Connection {
        repo,
        sink,
        states: HashMap::new(),
        welcomed: false,
    };
    loop {
        let result = select! {
            frame = stream.next() => match frame {
                Some(Ok(Message::Bytes(bytes))) => match conn.handle(&bytes, status).await {
                    Ok(None) => Ok(()),
                    Ok(Some(outcome)) => return outcome,
                    Err(()) => Err(()),
                },
                Some(Ok(Message::Text(_))) => Ok(()),
                Some(Err(_)) | None => Err(()),
            },
            id = changed.next() => match id {
                Some(id) if conn.welcomed => conn.local_change(id).await,
                _ => Ok(()),
            },
        };
        if result.is_err() {
            return Outcome::Closed {
                welcomed: conn.welcomed,
            };
        }
    }
}

struct Connection {
    repo: Repo,
    sink: Sink,
    /// Sync state for every subscribed list.
    states: HashMap<DocId, sync::State>,
    welcomed: bool,
}

/// `Err` means the socket failed.
type SendResult = Result<(), ()>;

impl Connection {
    async fn handle(
        &mut self,
        bytes: &[u8],
        status: RwSignal<SyncStatus>,
    ) -> Result<Option<Outcome>, ()> {
        use server_message::Msg;
        let Ok(message) = supp_shared::proto::sync::v1::ServerMessage::decode(bytes) else {
            logging::warn!("undecodable server message");
            return Ok(None);
        };
        match message.msg {
            Some(Msg::Welcome(welcome)) => {
                self.welcomed = true;
                status.set(SyncStatus::Online);
                for id in welcome
                    .doc_ids
                    .iter()
                    .filter_map(|b| DocId::from_bytes(b).ok())
                {
                    self.repo.ensure(id);
                }
                for id in self.repo.ids() {
                    self.subscribe(id).await?;
                }
            }
            Some(Msg::Sync(payload)) => {
                let Ok(id) = DocId::from_bytes(&payload.doc_id) else {
                    return Ok(None);
                };
                let Some(state) = self.states.get_mut(&id) else {
                    return Ok(None);
                };
                match sync::Message::decode(&payload.data) {
                    Ok(m) => {
                        if let Err(err) = self.repo.receive_sync(id, state, m) {
                            logging::error!("applying sync for {id}: {err}");
                        }
                    }
                    Err(err) => logging::error!("bad sync message for {id}: {err}"),
                }
                self.push(id).await?;
            }
            Some(Msg::Denied(denied)) => {
                if let Ok(id) = DocId::from_bytes(&denied.doc_id) {
                    // e.g. a list from another account kept on this device.
                    logging::warn!("not syncing {id}: {}", denied.reason);
                    self.states.remove(&id);
                }
            }
            Some(Msg::SessionEnded(_)) => return Ok(Some(Outcome::SessionEnded)),
            Some(Msg::Upgrade(_)) => return Ok(Some(Outcome::UpgradeRequired)),
            Some(Msg::Error(error)) => logging::warn!("server: {}", error.message),
            // A second Welcome, or a variant from a newer server.
            None => {}
        }
        Ok(None)
    }

    async fn local_change(&mut self, id: DocId) -> SendResult {
        if self.states.contains_key(&id) {
            self.push(id).await
        } else {
            // A list created while connected.
            self.subscribe(id).await
        }
    }

    async fn subscribe(&mut self, id: DocId) -> SendResult {
        self.states.insert(id, sync::State::new());
        let subscribe = client_message::Msg::Subscribe(Subscribe {
            doc_id: id.as_bytes().to_vec(),
        });
        send(&mut self.sink, subscribe).await?;
        self.push(id).await
    }

    /// Sends our next sync message for `id`, if the protocol has one.
    async fn push(&mut self, id: DocId) -> SendResult {
        let Some(state) = self.states.get_mut(&id) else {
            return Ok(());
        };
        let Some(message) = self.repo.generate_sync(id, state) else {
            return Ok(());
        };
        let payload = client_message::Msg::Sync(SyncPayload {
            doc_id: id.as_bytes().to_vec(),
            data: message.encode(),
        });
        send(&mut self.sink, payload).await
    }
}

async fn send(sink: &mut Sink, msg: client_message::Msg) -> SendResult {
    let bytes = ClientMessage { msg: Some(msg) }.encode_to_vec();
    sink.send(Message::Bytes(bytes)).await.map_err(|_| ())
}

fn sync_url() -> String {
    let location = window().location();
    let scheme = match location.protocol().as_deref() {
        Ok("https:") => "wss:",
        _ => "ws:",
    };
    format!("{scheme}//{}/sync", location.host().unwrap_or_default())
}
