//! Contents of a list document.
//!
//! Items are a map keyed by item ID and ordered by a fractional-index
//! `position`, rather than an Automerge list: moving an element of an
//! Automerge list is delete + insert, so concurrent moves would duplicate it.
//! Scalar fields are Automerge registers, so concurrent writes resolve to the
//! same last-writer-wins value on every peer.

use std::collections::HashMap;

use autosurgeon::{Hydrate, Reconcile};

/// Version of the document contents. Protobuf versions only the envelope.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Reconcile, Hydrate)]
pub struct ListDoc {
    #[autosurgeon(missing = "default_schema_version")]
    pub schema_version: u32,
    #[autosurgeon(missing = "Default::default")]
    pub title: String,
    #[autosurgeon(missing = "Default::default")]
    pub items: HashMap<String, Item>,
}

#[derive(Clone, Debug, PartialEq, Reconcile, Hydrate)]
pub struct Item {
    #[autosurgeon(missing = "Default::default")]
    pub text: String,
    #[autosurgeon(missing = "Default::default")]
    pub done: bool,
    #[autosurgeon(missing = "Default::default")]
    pub position: String,
}

fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

impl Default for ListDoc {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            title: String::new(),
            items: HashMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use automerge::{AutoCommit, sync::SyncDoc};
    use autosurgeon::{hydrate, reconcile};

    use super::*;

    fn item(text: &str, position: &str) -> Item {
        Item {
            text: text.into(),
            done: false,
            position: position.into(),
        }
    }

    fn sync(a: &mut AutoCommit, b: &mut AutoCommit) {
        let (mut sa, mut sb) = Default::default();
        loop {
            let ma = a.sync().generate_sync_message(&mut sa);
            let mb = b.sync().generate_sync_message(&mut sb);
            if ma.is_none() && mb.is_none() {
                break;
            }
            if let Some(m) = ma {
                b.sync().receive_sync_message(&mut sb, m).unwrap();
            }
            if let Some(m) = mb {
                a.sync().receive_sync_message(&mut sa, m).unwrap();
            }
        }
    }

    #[test]
    fn hydrate_fills_missing_fields() {
        let doc = AutoCommit::new();
        let list: ListDoc = hydrate(&doc).unwrap();
        assert_eq!(list, ListDoc::default());
    }

    #[test]
    fn round_trip() {
        let mut list = ListDoc {
            title: "groceries".into(),
            ..Default::default()
        };
        list.items.insert("a".into(), item("milk", "a0"));
        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &list).unwrap();
        assert_eq!(hydrate::<_, ListDoc>(&doc).unwrap(), list);
    }

    #[test]
    fn concurrent_edits_converge() {
        let mut base = ListDoc::default();
        base.items.insert("a".into(), item("milk", "a0"));
        let mut a = AutoCommit::new();
        reconcile(&mut a, &base).unwrap();
        let mut b = a.fork();

        // Same field edited on both sides, plus an independent insert on each.
        let mut la: ListDoc = hydrate(&a).unwrap();
        la.items.get_mut("a").unwrap().text = "oat milk".into();
        la.items.insert("b".into(), item("eggs", "a1"));
        reconcile(&mut a, &la).unwrap();

        let mut lb: ListDoc = hydrate(&b).unwrap();
        lb.items.get_mut("a").unwrap().text = "whole milk".into();
        lb.items.get_mut("a").unwrap().done = true;
        lb.items.insert("c".into(), item("bread", "a2"));
        reconcile(&mut b, &lb).unwrap();

        sync(&mut a, &mut b);

        let la: ListDoc = hydrate(&a).unwrap();
        let lb: ListDoc = hydrate(&b).unwrap();
        assert_eq!(la, lb);
        assert_eq!(la.items.len(), 3);
        let milk = &la.items["a"];
        assert!(milk.text == "oat milk" || milk.text == "whole milk");
        assert!(milk.done);
    }
}
