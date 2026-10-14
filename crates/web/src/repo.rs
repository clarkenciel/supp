//! Open documents, their persistence, and the signals the UI renders from.
//!
//! Every edit goes through [`Repo::change`]: apply to a hydrated `ListDoc`,
//! reconcile into the Automerge document, publish the re-hydrated value, and
//! persist the new changes. Writes per document are serialised so chunks
//! land in order and a failed write is retried by the next one.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use automerge::{AutoCommit, ChangeHash};
use autosurgeon::{hydrate, reconcile};
use leptos::{logging, prelude::*, task::spawn_local};
use supp_shared::{DocId, schema::ListDoc};
use wasm_bindgen::JsValue;

use crate::storage::{Storage, StorageError};

/// Change chunks a document may accumulate before it is compacted.
const COMPACT_AFTER: usize = 50;

/// Cheap handle to the shared repo; `Copy`, so it can live in context.
///
/// Create it outside any reactive owner (before mounting) so it lives for the
/// whole app. Per-document views are `ArcRwSignal`s for the same reason: an
/// arena signal would belong to whichever component happened to open or
/// create the document, and die when that component unmounts.
#[derive(Clone, Copy)]
pub struct Repo {
    inner: StoredValue<Rc<Inner>, LocalStorage>,
    lists: RwSignal<Vec<DocId>>,
}

struct Inner {
    storage: Storage,
    docs: RefCell<HashMap<DocId, OpenDoc>>,
}

struct OpenDoc {
    doc: AutoCommit,
    view: ArcRwSignal<ListDoc>,
    /// Heads of what is in IndexedDB; the next write saves everything after.
    persisted_heads: Vec<ChangeHash>,
    /// Chunks stored since the last snapshot.
    chunks: Vec<JsValue>,
    writing: bool,
    dirty: bool,
}

impl Repo {
    /// Opens storage and loads every local list. Lists are small and few, so
    /// loading them all up front keeps the overview simple.
    pub async fn open() -> Result<Self, StorageError> {
        let storage = Storage::open().await?;
        let ids = storage.list_docs().await?;
        let mut docs = HashMap::new();
        for &id in &ids {
            let mut loaded = storage.load(id).await?;
            let persisted = loaded.doc.get_heads();
            docs.insert(id, OpenDoc::new(id, loaded.doc, loaded.chunks, persisted));
        }
        Ok(Self {
            inner: StoredValue::new_local(Rc::new(Inner {
                storage,
                docs: RefCell::new(docs),
            })),
            lists: RwSignal::new(ids),
        })
    }

    /// Local lists, oldest first.
    pub fn lists(&self) -> Signal<Vec<DocId>> {
        self.lists.into()
    }

    pub fn list(&self, id: DocId) -> Option<Signal<ListDoc>> {
        let inner = self.inner.get_value();
        let docs = inner.docs.borrow();
        docs.get(&id).map(|d| d.view.clone().into())
    }

    pub fn create_list(&self, title: String) -> DocId {
        let id = DocId::new();
        let mut doc = AutoCommit::new();
        let list = ListDoc {
            title,
            ..Default::default()
        };
        reconcile(&mut doc, &list).expect("reconcile into empty doc");
        let inner = self.inner.get_value();
        // Nothing persisted yet, so the first write saves the whole doc.
        inner
            .docs
            .borrow_mut()
            .insert(id, OpenDoc::new(id, doc, Vec::new(), Vec::new()));
        self.lists.update(|l| l.push(id));

        spawn_local(async move {
            if let Err(err) = inner.storage.create_doc(id, js_sys::Date::now()).await {
                logging::error!("creating {id}: {err}");
            }
            persist(inner, id).await;
        });
        id
    }

    /// Applies a local edit.
    pub fn change(&self, id: DocId, f: impl FnOnce(&mut ListDoc)) {
        let inner = self.inner.get_value();
        {
            let mut docs = inner.docs.borrow_mut();
            let Some(open) = docs.get_mut(&id) else {
                return;
            };
            let mut list = open.view.get_untracked();
            f(&mut list);
            if let Err(err) = reconcile(&mut open.doc, &list) {
                logging::error!("reconciling {id}: {err}");
                return;
            }
            open.view.set(hydrate_or_default(&open.doc, id));
        }
        spawn_local(persist(inner, id));
    }

    /// A write already in flight may land after the delete; its chunk is
    /// orphaned (no `docs` entry) and never loaded.
    pub fn delete_list(&self, id: DocId) {
        let inner = self.inner.get_value();
        inner.docs.borrow_mut().remove(&id);
        self.lists.update(|l| l.retain(|&d| d != id));
        spawn_local(async move {
            if let Err(err) = inner.storage.delete_doc(id).await {
                logging::error!("deleting {id}: {err}");
            }
        });
    }
}

impl OpenDoc {
    fn new(
        id: DocId,
        doc: AutoCommit,
        chunks: Vec<JsValue>,
        persisted_heads: Vec<ChangeHash>,
    ) -> Self {
        Self {
            view: ArcRwSignal::new(hydrate_or_default(&doc, id)),
            persisted_heads,
            doc,
            chunks,
            writing: false,
            dirty: false,
        }
    }
}

fn hydrate_or_default(doc: &AutoCommit, id: DocId) -> ListDoc {
    hydrate(doc).unwrap_or_else(|err| {
        logging::error!("hydrating {id}: {err}");
        ListDoc::default()
    })
}

/// Writes everything after the persisted heads, looping while edits arrive
/// during the write. Only one write per document runs at a time.
async fn persist(inner: Rc<Inner>, id: DocId) {
    {
        let mut docs = inner.docs.borrow_mut();
        let Some(open) = docs.get_mut(&id) else {
            return;
        };
        if open.writing {
            open.dirty = true;
            return;
        }
        open.writing = true;
    }
    loop {
        let (bytes, heads) = {
            let mut docs = inner.docs.borrow_mut();
            let Some(open) = docs.get_mut(&id) else {
                return;
            };
            open.dirty = false;
            let heads = open.doc.get_heads();
            if heads == open.persisted_heads {
                open.writing = false;
                return;
            }
            (open.doc.save_after(&open.persisted_heads), heads)
        };
        let stored = inner.storage.append(id, &bytes).await;

        let compact = {
            let mut docs = inner.docs.borrow_mut();
            let Some(open) = docs.get_mut(&id) else {
                return;
            };
            match stored {
                Ok(chunk) => {
                    open.persisted_heads = heads;
                    open.chunks.push(chunk);
                }
                Err(err) => {
                    logging::error!("saving {id}: {err}");
                    open.writing = false;
                    return;
                }
            }
            (open.chunks.len() >= COMPACT_AFTER)
                .then(|| (open.doc.save(), std::mem::take(&mut open.chunks)))
        };
        if let Some((snapshot, chunks)) = compact
            && let Err(err) = inner.storage.compact(id, &snapshot, &chunks).await
        {
            logging::error!("compacting {id}: {err}");
            // Keep tracking the chunks so a later compaction removes them.
            if let Some(open) = inner.docs.borrow_mut().get_mut(&id) {
                open.chunks.extend(chunks);
            }
        }

        let mut docs = inner.docs.borrow_mut();
        let Some(open) = docs.get_mut(&id) else {
            return;
        };
        if !open.dirty {
            open.writing = false;
            return;
        }
    }
}
