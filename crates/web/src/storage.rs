//! IndexedDB persistence for list documents.
//!
//! A document is stored as an optional snapshot plus append-only change
//! chunks (output of `Automerge::save_after`). Chunks get random keys, so
//! several tabs can append to the same document without coordinating, and
//! Automerge accepts them in any order when loading.

use automerge::AutoCommit;
use idb::{Database, DatabaseEvent, Factory, KeyRange, ObjectStoreParams, Query, TransactionMode};
use js_sys::{Array, Uint8Array};
use prost::Message as _;
use supp_shared::{DocId, proto::api::v1::Me};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

const DB_NAME: &str = "supp";
/// v1: docs, snapshots, changes. v2: meta. v3: left.
const DB_VERSION: u32 = 3;

/// doc id → creation time (ms since epoch). The set of local lists.
const DOCS: &str = "docs";
/// doc id → full `Automerge::save` output.
const SNAPSHOTS: &str = "snapshots";
/// [doc id, chunk id] → incremental save output.
const CHANGES: &str = "changes";
/// doc id → true: lists deleted here whose Leave the server hasn't
/// confirmed yet. Survives going offline, so the delete still reaches the
/// account (and the list doesn't come back from the next Welcome).
const LEFT: &str = "left";
/// Small device-level records, keyed by name.
const META: &str = "meta";
/// META key: the account (protobuf `Me`) this device's lists belong to.
/// Absent while the device has only been used anonymously.
const ACCOUNT: &str = "account";

/// Asks the browser not to evict this origin's data under storage pressure,
/// which it otherwise may for sites that aren't installed. Best effort:
/// browsers decide by their own heuristics (Firefox asks the user).
pub async fn request_persistence() {
    let navigator = leptos::prelude::window().navigator();
    // Missing outside secure contexts.
    if !js_sys::Reflect::has(&navigator, &JsValue::from_str("storage")).unwrap_or(false) {
        return;
    }
    let storage = navigator.storage();
    let ask = async {
        if let Ok(true) = JsFuture::from(storage.persisted()?)
            .await
            .map(|v| v.is_truthy())
        {
            return Ok(true);
        }
        JsFuture::from(storage.persist()?)
            .await
            .map(|v| v.is_truthy())
    };
    match ask.await {
        Ok(granted) => leptos::logging::log!("persistent storage: {granted}"),
        Err(err) => leptos::logging::warn!("requesting persistent storage: {err:?}"),
    }
}

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

/// A document loaded from storage, with the ids of the change chunks folded
/// into it so a later compaction removes exactly those.
pub struct Loaded {
    pub doc: AutoCommit,
    pub chunks: Vec<String>,
}

/// A list's stored bytes: its snapshot, if any, and change chunks by id.
pub struct Stored {
    snapshot: Option<Vec<u8>>,
    chunks: Vec<(String, Vec<u8>)>,
}

impl Stored {
    /// Just one chunk.
    pub fn chunk(id: String, bytes: Vec<u8>) -> Self {
        Self {
            snapshot: None,
            chunks: vec![(id, bytes)],
        }
    }

    /// Loads everything into `doc` and returns the ids of the chunks.
    /// Automerge skips changes the document already has.
    pub fn load_into(self, doc: &mut AutoCommit) -> Result<Vec<String>> {
        if let Some(snapshot) = &self.snapshot {
            doc.load_incremental(snapshot)?;
        }
        let mut ids = Vec::with_capacity(self.chunks.len());
        for (id, bytes) in self.chunks {
            doc.load_incremental(&bytes)?;
            ids.push(id);
        }
        Ok(ids)
    }
}

impl Storage {
    pub async fn open() -> Result<Self> {
        let mut request = Factory::new()?.open(DB_NAME, Some(DB_VERSION))?;
        request.on_upgrade_needed(|event| {
            let db = event.database().expect("upgrade event has a database");
            let existing = db.store_names();
            // Every version only adds stores, so creating whichever are
            // missing upgrades from any earlier version.
            for name in [DOCS, SNAPSHOTS, CHANGES, META, LEFT] {
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

    /// Records a new list together with its initial contents, in one
    /// transaction, so a list never exists here without them.
    pub async fn create_doc(
        &self,
        id: DocId,
        created_at_ms: f64,
        snapshot: Option<&[u8]>,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction(&[DOCS, SNAPSHOTS], TransactionMode::ReadWrite)?;
        tx.object_store(DOCS)?
            .put(&created_at_ms.into(), Some(&doc_key(id)))?
            .await?;
        if let Some(snapshot) = snapshot {
            tx.object_store(SNAPSHOTS)?
                .put(&Uint8Array::from(snapshot).into(), Some(&doc_key(id)))?
                .await?;
        }
        tx.commit()?.await?;
        Ok(())
    }

    /// Loads a list, or `None` if it isn't (or is no longer) on this device.
    pub async fn load(&self, id: DocId) -> Result<Option<Loaded>> {
        let Some(stored) = self.read(id).await? else {
            return Ok(None);
        };
        // A fresh random actor ID, so each tab is its own peer.
        let mut doc = AutoCommit::new();
        let chunks = stored.load_into(&mut doc)?;
        Ok(Some(Loaded { doc, chunks }))
    }

    /// A list's stored bytes, or `None` if it isn't on this device.
    pub async fn read(&self, id: DocId) -> Result<Option<Stored>> {
        let tx = self
            .db
            .transaction(&[DOCS, SNAPSHOTS, CHANGES], TransactionMode::ReadOnly)?;
        if tx.object_store(DOCS)?.get(doc_key(id))?.await?.is_none() {
            return Ok(None);
        }
        let snapshot = tx.object_store(SNAPSHOTS)?.get(doc_key(id))?.await?;
        let changes = tx.object_store(CHANGES)?;
        let keys = changes.get_all_keys(Some(chunk_range(id)?), None)?.await?;
        let bytes = changes.get_all(Some(chunk_range(id)?), None)?.await?;
        tx.await?;

        let mut chunks = Vec::with_capacity(keys.len());
        for (key, bytes) in keys.iter().zip(&bytes) {
            let chunk = Array::from(key)
                .get(1)
                .as_string()
                .ok_or(StorageError::Corrupt("chunk key"))?;
            chunks.push((chunk, bytes_of(bytes)?));
        }
        Ok(Some(Stored {
            snapshot: snapshot.as_ref().map(bytes_of).transpose()?,
            chunks,
        }))
    }

    /// One change chunk, or `None` if it is gone: folded into a snapshot by
    /// a compaction, or deleted with its list.
    pub async fn read_chunk(&self, id: DocId, chunk: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.db.transaction(&[CHANGES], TransactionMode::ReadOnly)?;
        let bytes = tx.object_store(CHANGES)?.get(chunk_key(id, chunk))?.await?;
        tx.await?;
        bytes.as_ref().map(bytes_of).transpose()
    }

    /// Appends a change chunk and returns its id.
    pub async fn append(&self, id: DocId, bytes: &[u8]) -> Result<String> {
        let chunk = uuid::Uuid::new_v4().to_string();
        let tx = self
            .db
            .transaction(&[CHANGES], TransactionMode::ReadWrite)?;
        tx.object_store(CHANGES)?
            .put(
                &Uint8Array::from(bytes).into(),
                Some(&chunk_key(id, &chunk)),
            )?
            .await?;
        tx.commit()?.await?;
        Ok(chunk)
    }

    /// Replaces the snapshot and removes the chunks it subsumes. Chunks other
    /// tabs appended in the meantime are left alone.
    ///
    /// Another tab may have compacted changes this one never loaded into the
    /// stored snapshot, so `save` gets that snapshot to merge before saving
    /// the new one, inside the transaction so no other compaction can come
    /// between. If `save` returns `None`, nothing changes. Returns whether
    /// the snapshot was replaced.
    pub async fn compact(
        &self,
        id: DocId,
        chunks: &[String],
        save: impl FnOnce(Option<Vec<u8>>) -> Option<Vec<u8>>,
    ) -> Result<bool> {
        let tx = self
            .db
            .transaction(&[SNAPSHOTS, CHANGES], TransactionMode::ReadWrite)?;
        let snapshots = tx.object_store(SNAPSHOTS)?;
        let stored = snapshots
            .get(doc_key(id))?
            .await?
            .map(|s| bytes_of(&s))
            .transpose()?;
        let Some(snapshot) = save(stored) else {
            tx.commit()?.await?;
            return Ok(false);
        };
        snapshots
            .put(
                &Uint8Array::from(snapshot.as_slice()).into(),
                Some(&doc_key(id)),
            )?
            .await?;
        let changes = tx.object_store(CHANGES)?;
        for chunk in chunks {
            changes.delete(Query::Key(chunk_key(id, chunk)))?.await?;
        }
        tx.commit()?.await?;
        Ok(true)
    }

    /// Lists with a pending leave.
    pub async fn left_docs(&self) -> Result<Vec<DocId>> {
        let tx = self.db.transaction(&[LEFT], TransactionMode::ReadOnly)?;
        let keys = tx.object_store(LEFT)?.get_all_keys(None, None)?.await?;
        tx.await?;
        Ok(keys
            .iter()
            .filter_map(|k| k.as_string()?.parse().ok())
            .collect())
    }

    /// The server confirmed the leave.
    pub async fn forget_left(&self, id: DocId) -> Result<()> {
        let tx = self.db.transaction(&[LEFT], TransactionMode::ReadWrite)?;
        tx.object_store(LEFT)?.delete(doc_key(id))?.await?;
        tx.commit()?.await?;
        Ok(())
    }

    pub async fn account(&self) -> Result<Option<Me>> {
        let tx = self.db.transaction(&[META], TransactionMode::ReadOnly)?;
        let value = tx
            .object_store(META)?
            .get(JsValue::from_str(ACCOUNT))?
            .await?;
        tx.await?;
        value
            .map(|v| {
                Me::decode(bytes_of(&v)?.as_slice()).map_err(|_| StorageError::Corrupt("account"))
            })
            .transpose()
    }

    pub async fn set_account(&self, me: &Me) -> Result<()> {
        let tx = self.db.transaction(&[META], TransactionMode::ReadWrite)?;
        let bytes = Uint8Array::from(me.encode_to_vec().as_slice());
        tx.object_store(META)?
            .put(&bytes.into(), Some(&JsValue::from_str(ACCOUNT)))?
            .await?;
        tx.commit()?.await?;
        Ok(())
    }

    /// Erases everything: every list and the account record.
    pub async fn clear(&self) -> Result<()> {
        let stores = [DOCS, SNAPSHOTS, CHANGES, META, LEFT];
        let tx = self.db.transaction(&stores, TransactionMode::ReadWrite)?;
        for name in stores {
            tx.object_store(name)?.clear()?.await?;
        }
        tx.commit()?.await?;
        Ok(())
    }

    /// Deletes a list, recording a pending leave for it if `leave`.
    pub async fn delete_doc(&self, id: DocId, leave: bool) -> Result<()> {
        let tx = self.db.transaction(
            &[DOCS, SNAPSHOTS, CHANGES, LEFT],
            TransactionMode::ReadWrite,
        )?;
        if leave {
            tx.object_store(LEFT)?
                .put(&JsValue::TRUE, Some(&doc_key(id)))?
                .await?;
        }
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
