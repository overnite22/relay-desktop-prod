//! Moving and deleting several tree items at once (drag and drop, multiple
//! selection): checks and ordering, independent of the data source.

use crate::model::{Entry, Folder, Tree};

/// A selected node of the tree.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Item {
    Folder(String),
    Entry(String),
}

/// True when `folder` is `ancestor` or one of its sub-folders.
pub fn is_within(tree: &Tree, folder: &str, ancestor: &str) -> bool {
    let mut cur = Some(folder.to_owned());
    let mut guard = 0;
    while let Some(id) = cur {
        if id == ancestor {
            return true;
        }
        guard += 1;
        if guard > 256 {
            return false;
        }
        cur = tree.folders.iter().find(|f| f.id == id).and_then(|f| f.parent_id.clone());
    }
    false
}

/// Parent folder of an item (`None`: root or unknown item).
fn parent(tree: &Tree, item: &Item) -> Option<String> {
    match item {
        Item::Folder(id) => tree.folders.iter().find(|f| &f.id == id)?.parent_id.clone(),
        Item::Entry(id) => tree.entries.iter().find(|e| &e.id == id)?.folder_id.clone(),
    }
}

/// True when one of the folders of `items` (other than `item`) contains `item`:
/// it moves or disappears with that folder.
fn carried(tree: &Tree, items: &[Item], item: &Item) -> bool {
    let Some(start) = parent(tree, item) else {
        return false;
    };
    items.iter().any(|other| match other {
        Item::Folder(f) => other != item && is_within(tree, &start, f),
        Item::Entry(_) => false,
    })
}

/// Updated copies of the items to save for a move.
#[derive(Debug, Default)]
pub struct Plan {
    pub folders: Vec<Folder>,
    pub entries: Vec<Entry>,
}

impl Plan {
    pub fn len(&self) -> usize {
        self.folders.len() + self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Plans the move of `items` into `target` (`None`: root). Items already
/// there, or inside a moved folder, are left out; a folder cannot go into
/// itself or one of its sub-folders.
pub fn plan_move(tree: &Tree, items: &[Item], target: Option<&str>) -> Result<Plan, String> {
    if let Some(t) = target {
        if !tree.folders.iter().any(|f| f.id == t) {
            return Err("dossier de destination introuvable".into());
        }
    }
    let mut plan = Plan::default();
    for item in items {
        if carried(tree, items, item) {
            continue;
        }
        match item {
            Item::Folder(id) => {
                let Some(f) = tree.folders.iter().find(|f| &f.id == id) else {
                    continue;
                };
                if target.is_some_and(|t| is_within(tree, t, id)) {
                    return Err(format!("impossible de déplacer le dossier « {} » dans lui-même", f.name));
                }
                if f.parent_id.as_deref() != target && !plan.folders.iter().any(|x| &x.id == id) {
                    let mut f = f.clone();
                    f.parent_id = target.map(str::to_owned);
                    plan.folders.push(f);
                }
            }
            Item::Entry(id) => {
                let Some(e) = tree.entries.iter().find(|e| &e.id == id) else {
                    continue;
                };
                if e.folder_id.as_deref() != target && !plan.entries.iter().any(|x| &x.id == id) {
                    let mut e = e.clone();
                    e.folder_id = target.map(str::to_owned);
                    plan.entries.push(e);
                }
            }
        }
    }
    Ok(plan)
}

/// Deletion order of a multiple selection: entries first, then folders from
/// the deepest, so that a folder is empty when its turn comes.
pub fn delete_order(tree: &Tree, items: &[Item]) -> Vec<Item> {
    let depth = |id: &str| {
        let mut d = 0;
        let mut cur = tree.folders.iter().find(|f| f.id == id).and_then(|f| f.parent_id.clone());
        while let Some(p) = cur {
            d += 1;
            if d > 256 {
                break;
            }
            cur = tree.folders.iter().find(|f| f.id == p).and_then(|f| f.parent_id.clone());
        }
        d
    };
    let mut entries: Vec<Item> = items.iter().filter(|i| matches!(i, Item::Entry(_))).cloned().collect();
    let mut folders: Vec<(usize, Item)> = items
        .iter()
        .filter_map(|i| match i {
            Item::Folder(id) => Some((depth(id), i.clone())),
            Item::Entry(_) => None,
        })
        .collect();
    folders.sort_by_key(|f| std::cmp::Reverse(f.0));
    entries.dedup();
    entries.extend(folders.into_iter().map(|(_, i)| i));
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(id: &str, parent: Option<&str>) -> Folder {
        Folder {
            id: id.into(),
            parent_id: parent.map(Into::into),
            name: id.to_uppercase(),
            ..Default::default()
        }
    }

    fn entry(id: &str, folder: Option<&str>) -> Entry {
        let mut e = Entry::new(crate::model::Kind::Ssh);
        e.id = id.into();
        e.folder_id = folder.map(Into::into);
        e.name = id.into();
        e
    }

    // a ─ b ─ c ; entries x (a), y (c), z (root)
    fn tree() -> Tree {
        Tree {
            folders: vec![folder("a", None), folder("b", Some("a")), folder("c", Some("b"))],
            entries: vec![entry("x", Some("a")), entry("y", Some("c")), entry("z", None)],
        }
    }

    #[test]
    fn within() {
        let t = tree();
        assert!(is_within(&t, "c", "a"));
        assert!(is_within(&t, "a", "a"));
        assert!(!is_within(&t, "a", "c"));
    }

    #[test]
    fn move_entries_and_folders() {
        let t = tree();
        let p = plan_move(&t, &[Item::Entry("z".into()), Item::Folder("c".into())], Some("a")).unwrap();
        assert_eq!(p.entries.len(), 1);
        assert_eq!(p.entries[0].folder_id.as_deref(), Some("a"));
        assert_eq!(p.folders[0].parent_id.as_deref(), Some("a"));
        // To the root.
        let p = plan_move(&t, &[Item::Folder("b".into())], None).unwrap();
        assert_eq!(p.folders[0].parent_id, None);
    }

    #[test]
    fn move_skips_noop_and_carried_items() {
        let t = tree();
        // z is already at the root; y moves along with b.
        let items = [Item::Entry("z".into()), Item::Folder("b".into()), Item::Entry("y".into())];
        let p = plan_move(&t, &items, None).unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(p.folders[0].id, "b");
        assert!(plan_move(&t, &[Item::Entry("x".into())], Some("a")).unwrap().is_empty());
    }

    #[test]
    fn move_refuses_cycles_and_unknown_target() {
        let t = tree();
        assert!(plan_move(&t, &[Item::Folder("a".into())], Some("c")).is_err());
        assert!(plan_move(&t, &[Item::Folder("b".into())], Some("b")).is_err());
        assert!(plan_move(&t, &[Item::Entry("z".into())], Some("nope")).is_err());
    }

    #[test]
    fn deletion_order() {
        let t = tree();
        let items = [Item::Folder("a".into()), Item::Entry("x".into()), Item::Folder("c".into())];
        assert_eq!(
            delete_order(&t, &items),
            vec![Item::Entry("x".into()), Item::Folder("c".into()), Item::Folder("a".into())]
        );
    }
}
