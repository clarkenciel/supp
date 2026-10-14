//! Open documents, their persistence, and the signals the UI renders from.
//!
//! Every edit goes through [`Repo::change`]: apply to a hydrated `ListDoc`,
//! reconcile into the Automerge document, publish the re-hydrated value, and
//! persist the new changes. Writes per document are serialised so chunks
//! land in order and a failed write is retried by the next one.
//!
//! Syncing goes through [`Repo::generate_sync`] and [`Repo::receive_sync`];
//! the sync client hears about local edits via [`Repo::set_change_listener`].
//!
//! Other tabs are peers sharing the same IndexedDB. After each write the
//! repo tells them what it wrote (see [`crate::tabs`]), and it loads what
//! they write in turn, so tabs agree without a server. Their edits reach
//! this tab's sync loop like local ones.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
};

use automerge::{
    AutoCommit, AutomergeError, ChangeHash,
    sync::{self, SyncDoc},
};
use autosurgeon::{hydrate, reconcile};
use futures::{StreamExt, channel::mpsc};
use leptos::{logging, prelude::*, task::spawn_local};
use supp_shared::{DocId, proto::api::v1::Me, schema::ListDoc};

use crate::{
    storage::{Storage, StorageError, Stored},
    tabs::{TabMessage, Tabs},
};

#[derive(Clone, Copy, Debug)]
pub enum LocalEvent {
    /// A list was created or edited on this device (perhaps in another tab).
    Changed(DocId),
    /// A list was deleted on this device.
    Deleted(DocId),
}

pub type ChangeListener = Rc<dyn Fn(LocalEvent)>;

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
    /// Told about every local edit, so it can be synced.
    on_change: RefCell<Option<ChangeListener>>,
    tabs: Tabs,
    /// Messages from other tabs, queued until [`Repo::follow_other_tabs`].
    from_tabs: RefCell<Option<mpsc::UnboundedReceiver<TabMessage>>>,
}

struct OpenDoc {
    doc: AutoCommit,
    view: ArcRwSignal<ListDoc>,
    /// Heads of what is in IndexedDB; the next write saves everything after.
    persisted_heads: Vec<ChangeHash>,
    /// Stored chunks this document has written or loaded: what the next
    /// compaction folds into the snapshot and deletes. Chunks are shared
    /// with other tabs, which may delete them first; deleting one twice is
    /// harmless.
    chunks: HashSet<String>,
    writing: bool,
    dirty: bool,
}

impl Repo {
    /// Opens storage and loads every local list. Lists are small and few, so
    /// loading them all up front keeps the overview simple.
    pub async fn open() -> Result<Self, StorageError> {
        let storage = Storage::open().await?;
        // Listening before reading means nothing written meanwhile is missed;
        // anything already read is skipped when its message comes.
        let (to_repo, from_tabs) = mpsc::unbounded();
        let tabs = Tabs::open(move |message| {
            let _ = to_repo.unbounded_send(message);
        });
        let mut ids = storage.list_docs().await?;
        let mut docs = HashMap::new();
        for &id in &ids {
            // Deleted by another tab since listing.
            let Some(mut loaded) = storage.load(id).await? else {
                continue;
            };
            let persisted = loaded.doc.get_heads();
            docs.insert(id, OpenDoc::new(id, loaded.doc, loaded.chunks, persisted));
        }
        ids.retain(|id| docs.contains_key(id));
        Ok(Self {
            inner: StoredValue::new_local(Rc::new(Inner {
                storage,
                docs: RefCell::new(docs),
                on_change: RefCell::new(None),
                tabs,
                from_tabs: RefCell::new(Some(from_tabs)),
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

    /// Whether a local list has any contents yet. A list added by
    /// [`ensure`](Self::ensure) has none until its first sync. Untracked.
    pub fn has_contents(&self, id: DocId) -> bool {
        let inner = self.inner.get_value();
        let mut docs = inner.docs.borrow_mut();
        docs.get_mut(&id)
            .is_some_and(|d| !d.doc.get_heads().is_empty())
    }

    pub fn create_list(&self, title: String) -> DocId {
        let id = DocId::new();
        let mut doc = AutoCommit::new();
        let list = ListDoc {
            title,
            ..Default::default()
        };
        reconcile(&mut doc, &list).expect("reconcile into empty doc");
        let snapshot = doc.save();
        let heads = doc.get_heads();
        let inner = self.inner.get_value();
        // Nothing persisted yet. Mark it as writing so edits made before the
        // creation lands queue behind it instead of racing it.
        let mut open = OpenDoc::new(id, doc, Vec::new(), Vec::new());
        open.writing = true;
        inner.docs.borrow_mut().insert(id, open);
        self.lists.update(|l| l.push(id));
        self.notify(LocalEvent::Changed(id));

        spawn_local(async move {
            let created = inner
                .storage
                .create_doc(id, js_sys::Date::now(), Some(&snapshot))
                .await;
            if created.is_ok() {
                inner.tabs.post(&TabMessage::Added(id));
            }
            if let Some(open) = inner.docs.borrow_mut().get_mut(&id) {
                open.writing = false;
                match created {
                    Ok(()) => open.persisted_heads = heads,
                    // persisted_heads stays empty, so the next write saves
                    // the whole document.
                    Err(err) => logging::error!("creating {id}: {err}"),
                }
            }
            persist(inner, id).await;
        });
        id
    }

    /// Adds an empty local copy of a list known only from the server, so it
    /// can be synced in. Returns whether it was new.
    pub fn ensure(&self, id: DocId) -> bool {
        let inner = self.inner.get_value();
        if inner.docs.borrow().contains_key(&id) {
            return false;
        }
        inner.docs.borrow_mut().insert(
            id,
            OpenDoc::new(id, AutoCommit::new(), Vec::new(), Vec::new()),
        );
        self.lists.update(|l| l.push(id));
        spawn_local(async move {
            match inner
                .storage
                .create_doc(id, js_sys::Date::now(), None)
                .await
            {
                Ok(()) => inner.tabs.post(&TabMessage::Added(id)),
                Err(err) => logging::error!("creating {id}: {err}"),
            }
        });
        true
    }

    /// Every local list, untracked.
    pub fn ids(&self) -> Vec<DocId> {
        self.lists.get_untracked()
    }

    pub fn set_change_listener(&self, listener: Option<ChangeListener>) {
        *self.inner.get_value().on_change.borrow_mut() = listener;
    }

    fn notify(&self, event: LocalEvent) {
        self.inner.get_value().notify(event);
    }

    pub async fn left_docs(&self) -> Result<Vec<DocId>, StorageError> {
        self.inner.get_value().storage.left_docs().await
    }

    pub async fn forget_left(&self, id: DocId) -> Result<(), StorageError> {
        self.inner.get_value().storage.forget_left(id).await
    }

    pub fn generate_sync(&self, id: DocId, state: &mut sync::State) -> Option<sync::Message> {
        let inner = self.inner.get_value();
        let mut docs = inner.docs.borrow_mut();
        docs.get_mut(&id)?.doc.sync().generate_sync_message(state)
    }

    /// Applies a sync message from the server; if it brought changes,
    /// refreshes the view and persists them.
    pub fn receive_sync(
        &self,
        id: DocId,
        state: &mut sync::State,
        message: sync::Message,
    ) -> Result<(), AutomergeError> {
        let inner = self.inner.get_value();
        let changed = {
            let mut docs = inner.docs.borrow_mut();
            let Some(open) = docs.get_mut(&id) else {
                return Ok(());
            };
            let before = open.doc.get_heads();
            open.doc.sync().receive_sync_message(state, message)?;
            let changed = open.doc.get_heads() != before;
            if changed {
                open.view.set(hydrate_or_default(&open.doc, id));
            }
            changed
        };
        if changed {
            spawn_local(persist(inner, id));
        }
        Ok(())
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
        self.notify(LocalEvent::Changed(id));
        spawn_local(persist(inner, id));
    }

    pub async fn account(&self) -> Result<Option<Me>, StorageError> {
        self.inner.get_value().storage.account().await
    }

    /// Records the account this device's lists belong to, and tells the
    /// other tabs.
    pub async fn set_account(&self, me: &Me) -> Result<(), StorageError> {
        let inner = self.inner.get_value();
        inner.storage.set_account(me).await?;
        inner.tabs.post(&TabMessage::Account(Some(me.user_id)));
        Ok(())
    }

    /// Tells the other tabs that this device has signed out (and so been
    /// erased).
    pub fn announce_signed_out(&self) {
        self.inner.get_value().tabs.post(&TabMessage::Account(None));
    }

    /// Forgets every list, on this device only. Other tabs are not told:
    /// erasing always comes with an account change, which the caller
    /// announces once it is complete.
    pub async fn erase_all(&self) -> Result<(), StorageError> {
        let inner = self.inner.get_value();
        inner.storage.clear().await?;
        inner.docs.borrow_mut().clear();
        self.lists.set(Vec::new());
        Ok(())
    }

    /// A write already in flight may land after the delete; its chunk is
    /// orphaned (no `docs` entry) and never loaded.
    pub fn delete_list(&self, id: DocId) {
        self.remove(id, true);
        self.notify(LocalEvent::Deleted(id));
    }

    /// Drops the local copy of a list the user left on another device.
    pub fn forget(&self, id: DocId) {
        self.remove(id, false);
    }

    fn remove(&self, id: DocId, leave: bool) {
        let inner = self.inner.get_value();
        inner.docs.borrow_mut().remove(&id);
        self.lists.update(|l| l.retain(|&d| d != id));
        spawn_local(async move {
            match inner.storage.delete_doc(id, leave).await {
                Ok(()) => inner.tabs.post(&TabMessage::Removed(id)),
                Err(err) => logging::error!("deleting {id}: {err}"),
            }
        });
    }

    /// Starts applying what other tabs write. Account changes go to
    /// `on_account`, since what they mean depends on this tab's session.
    /// Messages are handled one at a time, in order, so a list added and
    /// removed in quick succession can't come back.
    pub fn follow_other_tabs(&self, on_account: impl Fn(Option<i64>) + 'static) {
        let Some(mut messages) = self.inner.get_value().from_tabs.take() else {
            return;
        };
        let repo = *self;
        spawn_local(async move {
            while let Some(message) = messages.next().await {
                match message {
                    TabMessage::Chunk(id, chunk) => repo.load_chunk(id, chunk).await,
                    TabMessage::Added(id) => repo.load_list(id).await,
                    TabMessage::Removed(id) => repo.drop_removed(id),
                    TabMessage::Account(user) => on_account(user),
                }
            }
        });
    }

    /// Loads a chunk another tab stored. If it is gone, a compaction folded
    /// it into the snapshot (or the list was deleted, and a `Removed`
    /// follows), so reload the list's whole stored state instead.
    async fn load_chunk(&self, id: DocId, chunk: String) {
        let inner = self.inner.get_value();
        let unread = inner
            .docs
            .borrow()
            .get(&id)
            .is_some_and(|d| !d.chunks.contains(&chunk));
        if !unread {
            return;
        }
        let stored = match inner.storage.read_chunk(id, &chunk).await {
            Ok(Some(bytes)) => Ok(Some(Stored::chunk(chunk, bytes))),
            Ok(None) => inner.storage.read(id).await,
            Err(err) => Err(err),
        };
        match stored {
            Ok(Some(stored)) => self.merge(id, stored),
            Ok(None) => {}
            Err(err) => logging::error!("loading another tab's changes to {id}: {err}"),
        }
    }

    /// Applies stored changes to an open list: refreshes its view and tells
    /// the sync loop, as for a local edit.
    fn merge(&self, id: DocId, stored: Stored) {
        let inner = self.inner.get_value();
        let changed = {
            let mut docs = inner.docs.borrow_mut();
            let Some(open) = docs.get_mut(&id) else {
                return;
            };
            let before = open.doc.get_heads();
            let up_to_date = before == open.persisted_heads;
            match stored.load_into(&mut open.doc) {
                Ok(chunks) => open.chunks.extend(chunks),
                Err(err) => logging::error!("loading another tab's changes to {id}: {err}"),
            }
            let heads = open.doc.get_heads();
            let changed = heads != before;
            if changed {
                open.view.set(hydrate_or_default(&open.doc, id));
                // Already in IndexedDB, so not worth writing again. With
                // writes of our own pending they are, harmlessly: there's
                // no telling apart which changes are stored.
                if up_to_date && !open.writing {
                    open.persisted_heads = heads;
                }
            }
            changed
        };
        if changed {
            self.notify(LocalEvent::Changed(id));
        }
    }

    /// Opens a list another tab added.
    async fn load_list(&self, id: DocId) {
        let inner = self.inner.get_value();
        if inner.docs.borrow().contains_key(&id) {
            return;
        }
        let mut loaded = match inner.storage.load(id).await {
            Ok(Some(loaded)) => loaded,
            // Removed again already.
            Ok(None) => return,
            Err(err) => return logging::error!("loading {id} from another tab: {err}"),
        };
        {
            let mut docs = inner.docs.borrow_mut();
            // Added here meanwhile, from this tab's own sync loop; its
            // contents arrive like any other tab's changes.
            if docs.contains_key(&id) {
                return;
            }
            let persisted = loaded.doc.get_heads();
            docs.insert(id, OpenDoc::new(id, loaded.doc, loaded.chunks, persisted));
        }
        self.lists.update(|l| l.push(id));
        // So a signed-in tab subscribes to it.
        self.notify(LocalEvent::Changed(id));
    }

    /// Another tab removed a list from storage; stop showing it. That tab
    /// also tells the server, if there is anything to tell.
    fn drop_removed(&self, id: DocId) {
        let inner = self.inner.get_value();
        if inner.docs.borrow_mut().remove(&id).is_some() {
            self.lists.update(|l| l.retain(|&d| d != id));
        }
    }
}

impl Inner {
    fn notify(&self, event: LocalEvent) {
        let listener = self.on_change.borrow().clone();
        if let Some(listener) = listener {
            listener(event);
        }
    }
}

impl OpenDoc {
    fn new(
        id: DocId,
        doc: AutoCommit,
        chunks: Vec<String>,
        persisted_heads: Vec<ChangeHash>,
    ) -> Self {
        Self {
            view: ArcRwSignal::new(hydrate_or_default(&doc, id)),
            persisted_heads,
            doc,
            chunks: chunks.into_iter().collect(),
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

/// Replaces the stored snapshot with one of this tab's document, after
/// merging in the stored one: another tab may have compacted changes into it
/// that this tab never loaded, and overwriting it would lose them.
async fn compact_doc(inner: &Inner, id: DocId, chunks: Vec<String>) {
    let mut saved = None;
    let mut merged = false;
    let compacted = inner
        .storage
        .compact(id, &chunks, |stored| {
            let mut docs = inner.docs.borrow_mut();
            let open = docs.get_mut(&id)?;
            if let Some(stored) = stored {
                let before = open.doc.get_heads();
                if let Err(err) = open.doc.load_incremental(&stored) {
                    // Overwriting what we can't read could lose changes.
                    logging::error!("merging the stored snapshot of {id}: {err}");
                    return None;
                }
                if open.doc.get_heads() != before {
                    open.view.set(hydrate_or_default(&open.doc, id));
                    merged = true;
                }
            }
            saved = Some(open.doc.get_heads());
            Some(open.doc.save())
        })
        .await;
    let mut docs = inner.docs.borrow_mut();
    let Some(open) = docs.get_mut(&id) else {
        return;
    };
    match compacted {
        // Everything up to the saved heads is stored now.
        Ok(true) => open.persisted_heads = saved.expect("saved when compacted"),
        Ok(false) => open.chunks.extend(chunks),
        Err(err) => {
            logging::error!("compacting {id}: {err}");
            // Keep tracking the chunks so a later compaction removes them.
            open.chunks.extend(chunks);
        }
    }
    drop(docs);
    if merged {
        // Changes this tab hadn't seen; the server may not have them either.
        inner.notify(LocalEvent::Changed(id));
    }
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
                    inner.tabs.post(&TabMessage::Chunk(id, chunk.clone()));
                    open.chunks.insert(chunk);
                }
                Err(err) => {
                    logging::error!("saving {id}: {err}");
                    open.writing = false;
                    return;
                }
            }
            (open.chunks.len() >= COMPACT_AFTER)
                .then(|| std::mem::take(&mut open.chunks).into_iter().collect())
        };
        if let Some(chunks) = compact {
            compact_doc(&inner, id, chunks).await;
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
