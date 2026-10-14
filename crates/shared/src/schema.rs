//! Contents of a list document.
//!
//! Items are a map keyed by item ID and ordered by a fractional-index
//! `position`, rather than an Automerge list: moving an element of an
//! Automerge list is delete + insert, so concurrent moves would duplicate it.
//! Scalar fields are Automerge registers, so concurrent writes resolve to the
//! same last-writer-wins value on every peer.

use std::collections::HashMap;

use autosurgeon::{Hydrate, Reconcile};
use fractional_index::FractionalIndex;

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

impl ListDoc {
    /// Items in display order: by position, with ties broken by ID. Ties
    /// happen when two peers insert at the same spot concurrently.
    pub fn ordered(&self) -> Vec<(&str, &Item)> {
        let mut items: Vec<_> = self.items.iter().map(|(id, i)| (id.as_str(), i)).collect();
        items.sort_by(|(a_id, a), (b_id, b)| (&a.position, a_id).cmp(&(&b.position, b_id)));
        items
    }

    /// Position that puts an item at `index` of [`Self::ordered`], ignoring
    /// the item `moving` (if any) so it can be repositioned.
    pub fn position_at(&self, index: usize, moving: Option<&str>) -> String {
        let others: Vec<_> = self
            .ordered()
            .into_iter()
            .filter(|(id, _)| Some(*id) != moving)
            .map(|(_, item)| parse(&item.position))
            .collect();
        let index = index.min(others.len());
        let before = index.checked_sub(1).and_then(|i| others[i].as_ref());
        let after = others.get(index).and_then(Option::as_ref);
        let position = FractionalIndex::new(before, after)
            // Equal or out-of-order neighbours (a concurrent-insert tie, or
            // positions from a misbehaving peer): land just after `before`.
            .or_else(|| before.map(FractionalIndex::new_after))
            .unwrap_or_default();
        position.to_string()
    }
}

/// Positions that don't parse are ignored as neighbours. They still sort,
/// since the string encoding preserves byte order.
fn parse(position: &str) -> Option<FractionalIndex> {
    FractionalIndex::from_string(position).ok()
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

    fn order(list: &ListDoc) -> Vec<&str> {
        list.ordered().into_iter().map(|(id, _)| id).collect()
    }

    fn insert_at(list: &mut ListDoc, id: &str, index: usize) {
        let position = list.position_at(index, None);
        list.items.insert(id.into(), item(id, &position));
    }

    #[test]
    fn positions_insert_anywhere() {
        let mut list = ListDoc::default();
        insert_at(&mut list, "b", 0);
        insert_at(&mut list, "d", 1);
        insert_at(&mut list, "a", 0);
        insert_at(&mut list, "c", 2);
        insert_at(&mut list, "e", 99);
        assert_eq!(order(&list), ["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn positions_move() {
        let mut list = ListDoc::default();
        for (i, id) in ["a", "b", "c", "d"].into_iter().enumerate() {
            insert_at(&mut list, id, i);
        }
        let to_front = list.position_at(0, Some("c"));
        list.items.get_mut("c").unwrap().position = to_front;
        assert_eq!(order(&list), ["c", "a", "b", "d"]);
        let to_end = list.position_at(3, Some("a"));
        list.items.get_mut("a").unwrap().position = to_end;
        assert_eq!(order(&list), ["c", "b", "d", "a"]);
    }

    #[test]
    fn positions_survive_repeated_inserts_at_one_spot() {
        let mut list = ListDoc::default();
        insert_at(&mut list, "first", 0);
        insert_at(&mut list, "last", 1);
        for i in 0..200 {
            insert_at(&mut list, &format!("m{i:03}"), 1);
        }
        let ids = order(&list);
        assert_eq!(ids.len(), 202);
        assert_eq!(ids[0], "first");
        assert_eq!(ids[1], "m199");
        assert_eq!(ids[201], "last");
    }

    #[test]
    fn tied_positions_order_by_id_and_still_accept_inserts() {
        let mut list = ListDoc::default();
        let p = list.position_at(0, None);
        list.items.insert("y".into(), item("y", &p));
        list.items.insert("x".into(), item("x", &p));
        assert_eq!(order(&list), ["x", "y"]);
        insert_at(&mut list, "z", 1);
        assert_eq!(list.items.len(), 3);
        assert_eq!(order(&list)[0], "x");
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
