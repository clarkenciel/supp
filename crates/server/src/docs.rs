//! Open documents and their persistence.
//!
//! `Docs` is the in-memory registry of open documents; `DocStore` keeps each
//! one in SQLite as a snapshot plus the incremental changes since it.
//!
//! Every document a connection subscribes to is loaded once and shared. Other
//! connections learn about changes through a `watch` channel of the document's
//! heads; at this scale an in-process channel replaces a pub/sub system.

use std::{collections::HashMap, sync::Arc};

use automerge::{
    Automerge, ChangeHash,
    sync::{self, SyncDoc},
};
use sqlx::SqlitePool;
use supp_shared::DocId;
use tokio::sync::{Mutex, watch};

/// Change rows a document may accumulate before it is compacted.
pub const DEFAULT_COMPACT_AFTER: usize = 100;

pub struct Docs {
    store: DocStore,
    compact_after: usize,
    // Held across loads so concurrent subscribers never load a document twice.
    open: Mutex<HashMap<DocId, Arc<DocHandle>>>,
}

impl Docs {
    pub fn new(store: DocStore, compact_after: usize) -> Self {
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
    store: DocStore,
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

#[derive(Clone)]
pub struct DocStore {
    pool: SqlitePool,
}

/// A document as loaded from disk, with the number of change rows stacked on
/// its snapshot.
pub struct Loaded {
    pub doc: Automerge,
    pub change_rows: usize,
}

impl DocStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Loads a document, or `None` if nothing has been stored for it.
    pub async fn load(&self, id: DocId) -> anyhow::Result<Option<Loaded>> {
        let id = id.as_bytes().as_slice();
        let snapshot: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM doc_snapshots WHERE doc_id = ?")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let changes: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT bytes FROM doc_changes WHERE doc_id = ? ORDER BY seq")
                .bind(id)
                .fetch_all(&self.pool)
                .await?;
        if snapshot.is_none() && changes.is_empty() {
            return Ok(None);
        }
        let mut doc = match snapshot {
            Some(bytes) => Automerge::load(&bytes)?,
            None => Automerge::new(),
        };
        for bytes in &changes {
            doc.load_incremental(bytes)?;
        }
        Ok(Some(Loaded {
            doc,
            change_rows: changes.len(),
        }))
    }

    /// Appends incremental changes (from `Automerge::save_after`).
    pub async fn append(&self, id: DocId, bytes: &[u8]) -> anyhow::Result<()> {
        let id = id.as_bytes().as_slice();
        sqlx::query(
            "INSERT INTO doc_changes (doc_id, seq, bytes) VALUES (?1, \
             COALESCE((SELECT MAX(seq) FROM doc_changes WHERE doc_id = ?1), 0) + 1, ?2)",
        )
        .bind(id)
        .bind(bytes)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the snapshot with a full save and drops the change rows.
    pub async fn compact(&self, id: DocId, snapshot: &[u8]) -> anyhow::Result<()> {
        let id = id.as_bytes().as_slice();
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO doc_snapshots (doc_id, bytes) VALUES (?1, ?2) \
             ON CONFLICT (doc_id) DO UPDATE SET bytes = excluded.bytes",
        )
        .bind(id)
        .bind(snapshot)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM doc_changes WHERE doc_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn change_rows(&self, id: DocId) -> anyhow::Result<usize> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM doc_changes WHERE doc_id = ?")
            .bind(id.as_bytes().as_slice())
            .fetch_one(&self.pool)
            .await?;
        Ok(n as usize)
    }
}
