//! In-memory registry of open documents.
//!
//! Every document a connection subscribes to is loaded once and shared. Other
//! connections learn about changes through a `watch` channel of the document's
//! heads; at this scale an in-process channel replaces a pub/sub system.

use std::{collections::HashMap, sync::Arc};

use automerge::{
    Automerge, ChangeHash,
    sync::{self, SyncDoc},
};
use supp_shared::DocId;
use tokio::sync::{Mutex, watch};

use crate::store::Store;

/// Change rows a document may accumulate before it is compacted.
pub const DEFAULT_COMPACT_AFTER: usize = 100;

pub struct Docs {
    store: Store,
    compact_after: usize,
    // Held across loads so concurrent subscribers never load a document twice.
    open: Mutex<HashMap<DocId, Arc<DocHandle>>>,
}

impl Docs {
    pub fn new(store: Store, compact_after: usize) -> Self {
        Self {
            store,
            compact_after,
            open: Mutex::default(),
        }
    }

    pub async fn get(&self, id: DocId) -> anyhow::Result<Arc<DocHandle>> {
        let mut open = self.open.lock().await;
        if let Some(handle) = open.get(&id) {
            return Ok(handle.clone());
        }
        let (doc, change_rows) = match self.store.load(id).await? {
            Some(loaded) => (loaded.doc, loaded.change_rows),
            None => (Automerge::new(), 0),
        };
        let heads = doc.get_heads();
        let handle = Arc::new(DocHandle {
            id,
            store: self.store.clone(),
            compact_after: self.compact_after,
            heads: watch::Sender::new(heads.clone()),
            inner: Mutex::new(Inner {
                doc,
                persisted_heads: heads,
                change_rows,
            }),
        });
        open.insert(id, handle.clone());
        Ok(handle)
    }
}

pub struct DocHandle {
    id: DocId,
    store: Store,
    compact_after: usize,
    heads: watch::Sender<Vec<ChangeHash>>,
    inner: Mutex<Inner>,
}

struct Inner {
    doc: Automerge,
    /// Heads of the state on disk. Saving everything after these (rather than
    /// after the heads before the latest message) means a failed write is
    /// retried by the next one.
    persisted_heads: Vec<ChangeHash>,
    change_rows: usize,
}

impl DocHandle {
    pub fn id(&self) -> DocId {
        self.id
    }

    /// Notified whenever the document's heads change.
    pub fn watch_heads(&self) -> watch::Receiver<Vec<ChangeHash>> {
        self.heads.subscribe()
    }

    /// Applies a peer's sync message, persists any new changes, notifies
    /// other subscribers, and returns the reply for that peer, if any.
    pub async fn receive(
        &self,
        state: &mut sync::State,
        message: sync::Message,
    ) -> anyhow::Result<Option<sync::Message>> {
        let mut inner = self.inner.lock().await;
        inner.doc.receive_sync_message(state, message)?;
        let heads = inner.doc.get_heads();
        if heads != inner.persisted_heads {
            // Persist before publishing: if this fails the peer still has the
            // changes and resends them on its next sync.
            let bytes = inner.doc.save_after(&inner.persisted_heads);
            self.store.append(self.id, &bytes).await?;
            inner.persisted_heads = heads.clone();
            inner.change_rows += 1;
            if inner.change_rows >= self.compact_after {
                self.store.compact(self.id, &inner.doc.save()).await?;
                inner.change_rows = 0;
            }
            self.heads.send_replace(heads);
        }
        Ok(inner.doc.generate_sync_message(state))
    }

    pub async fn generate(&self, state: &mut sync::State) -> Option<sync::Message> {
        self.inner.lock().await.doc.generate_sync_message(state)
    }
}
