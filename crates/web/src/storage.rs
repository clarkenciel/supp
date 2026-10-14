//! IndexedDB persistence for list documents.
//!
//! A document is stored as an optional snapshot plus append-only change
//! chunks (output of `Automerge::save_after`). Chunks get random keys, so
//! several tabs can append to the same document without coordinating, and
//! Automerge accepts them in any order when loading.

use automerge::AutoCommit;
use idb::{Database, DatabaseEvent, Factory, KeyRange, ObjectStoreParams, Query, TransactionMode};
use js_sys::{Array, Uint8Array};
use supp_shared::DocId;
use wasm_bindgen::{JsCast, JsValue};

const DB_NAME: &str = "supp";
const DB_VERSION: u32 = 1;

/// doc id → creation time (ms since epoch). The set of local lists.
const DOCS: &str = "docs";
/// doc id → full `Automerge::save` output.
const SNAPSHOTS: &str = "snapshots";
/// [doc id, chunk id] → incremental save output.
const CHANGES: &str = "changes";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("indexeddb: {0}")]
    Idb(#[from] idb::Error),
    #[error("automerge: {0}")]
    Automerge(#[from] automerge::AutomergeError),
    #[error("corrupt record: {0}")]
    Corrupt(&'static str),
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub struct Storage {
    db: Database,
}

/// A document loaded from storage, with the keys of the change chunks folded
/// into it so a later compaction removes exactly those.
pub struct Loaded {
    pub doc: AutoCommit,
    pub chunks: Vec<JsValue>,
}

impl Storage {
    pub async fn open() -> Result<Self> {
        let mut request = Factory::new()?.open(DB_NAME, Some(DB_VERSION))?;
        request.on_upgrade_needed(|event| {
            let db = event.database().expect("upgrade event has a database");
            let existing = db.store_names();
            for name in [DOCS, SNAPSHOTS, CHANGES] {
                if !existing.iter().any(|n| n == name) {
                    db.create_object_store(name, ObjectStoreParams::new())
                        .expect("create object store");
                }
            }
        });
        Ok(Self { db: request.await? })
    }

    /// Local lists, oldest first.
    pub async fn list_docs(&self) -> Result<Vec<DocId>> {
        let tx = self.db.transaction(&[DOCS], TransactionMode::ReadOnly)?;
        let store = tx.object_store(DOCS)?;
        let keys = store.get_all_keys(None, None)?.await?;
        let created = store.get_all(None, None)?.await?;
        tx.await?;
        let mut docs = Vec::with_capacity(keys.len());
        for (key, created) in keys.iter().zip(created) {
            let id = key
                .as_string()
                .and_then(|s| s.parse().ok())
                .ok_or(StorageError::Corrupt("doc key"))?;
            docs.push((created.as_f64().unwrap_or_default(), id));
        }
        docs.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(docs.into_iter().map(|(_, id)| id).collect())
    }

    pub async fn create_doc(&self, id: DocId, created_at_ms: f64) -> Result<()> {
        let tx = self.db.transaction(&[DOCS], TransactionMode::ReadWrite)?;
        tx.object_store(DOCS)?
            .put(&created_at_ms.into(), Some(&doc_key(id)))?
            .await?;
        tx.commit()?.await?;
        Ok(())
    }

    pub async fn load(&self, id: DocId) -> Result<Loaded> {
        let tx = self
            .db
            .transaction(&[SNAPSHOTS, CHANGES], TransactionMode::ReadOnly)?;
        let snapshot = tx.object_store(SNAPSHOTS)?.get(doc_key(id))?.await?;
        let changes = tx.object_store(CHANGES)?;
        let chunks = changes.get_all_keys(Some(chunk_range(id)?), None)?.await?;
        let bytes = changes.get_all(Some(chunk_range(id)?), None)?.await?;
        tx.await?;

        // Loading assigns a fresh random actor ID, so each tab is its own peer.
        let mut doc = match snapshot {
            Some(snapshot) => AutoCommit::load(&bytes_of(&snapshot)?)?,
            None => AutoCommit::new(),
        };
        for chunk in &bytes {
            doc.load_incremental(&bytes_of(chunk)?)?;
        }
        Ok(Loaded { doc, chunks })
    }

    /// Appends a change chunk and returns its key.
    pub async fn append(&self, id: DocId, bytes: &[u8]) -> Result<JsValue> {
        let key = chunk_key(id, &uuid::Uuid::new_v4().to_string());
        let tx = self
            .db
            .transaction(&[CHANGES], TransactionMode::ReadWrite)?;
        tx.object_store(CHANGES)?
            .put(&Uint8Array::from(bytes).into(), Some(&key))?
            .await?;
        tx.commit()?.await?;
        Ok(key)
    }

    /// Replaces the snapshot and removes the chunks it subsumes. Chunks other
    /// tabs appended in the meantime are left alone.
    pub async fn compact(&self, id: DocId, snapshot: &[u8], chunks: &[JsValue]) -> Result<()> {
        let tx = self
            .db
            .transaction(&[SNAPSHOTS, CHANGES], TransactionMode::ReadWrite)?;
        tx.object_store(SNAPSHOTS)?
            .put(&Uint8Array::from(snapshot).into(), Some(&doc_key(id)))?
            .await?;
        let changes = tx.object_store(CHANGES)?;
        for chunk in chunks {
            changes.delete(Query::Key(chunk.clone()))?.await?;
        }
        tx.commit()?.await?;
        Ok(())
    }

    pub async fn delete_doc(&self, id: DocId) -> Result<()> {
        let tx = self
            .db
            .transaction(&[DOCS, SNAPSHOTS, CHANGES], TransactionMode::ReadWrite)?;
        tx.object_store(DOCS)?.delete(doc_key(id))?.await?;
        tx.object_store(SNAPSHOTS)?.delete(doc_key(id))?.await?;
        tx.object_store(CHANGES)?.delete(chunk_range(id)?)?.await?;
        tx.commit()?.await?;
        Ok(())
    }
}

fn doc_key(id: DocId) -> JsValue {
    JsValue::from_str(&id.to_string())
}

fn chunk_key(id: DocId, chunk: &str) -> JsValue {
    Array::of2(&doc_key(id), &JsValue::from_str(chunk)).into()
}

/// All `[id, *]` chunk keys: arrays sort after strings in IndexedDB key order,
/// so `[id, []]` is above every `[id, "<chunk id>"]`.
fn chunk_range(id: DocId) -> Result<Query> {
    let lower = Array::of1(&doc_key(id));
    let upper = Array::of2(&doc_key(id), &Array::new());
    Ok(KeyRange::bound(&lower, &upper, None, None)?.into())
}

fn bytes_of(value: &JsValue) -> Result<Vec<u8>> {
    if value.is_instance_of::<Uint8Array>() {
        Ok(Uint8Array::new(value).to_vec())
    } else {
        Err(StorageError::Corrupt("expected bytes"))
    }
}
