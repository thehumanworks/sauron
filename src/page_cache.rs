use crate::snapshot_nodes::SnapshotNode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PageCacheEntry {
    pub snapshot_id: u64,
    pub nodes: Vec<SnapshotNode>,
    pub by_id: HashMap<String, SnapshotNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDelta {
    pub from_snapshot_id: u64,
    pub to_snapshot_id: u64,
    pub added: Vec<SnapshotNode>,
    pub removed: Vec<SnapshotNode>,
    pub changed: Vec<SnapshotNode>,
}

pub fn build_cache_entry(snapshot_id: u64, nodes: Vec<SnapshotNode>) -> PageCacheEntry {
    let mut by_id = HashMap::new();
    for node in &nodes {
        by_id.insert(node.id.clone(), node.clone());
    }
    PageCacheEntry {
        snapshot_id,
        nodes,
        by_id,
    }
}

pub fn diff_entries(prev: &PageCacheEntry, next: &PageCacheEntry) -> SnapshotDelta {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();

    for (id, node) in &next.by_id {
        match prev.by_id.get(id) {
            None => added.push(node.clone()),
            Some(prev_node) if prev_node != node => changed.push(node.clone()),
            _ => {}
        }
    }

    for (id, node) in &prev.by_id {
        if !next.by_id.contains_key(id) {
            removed.push(node.clone());
        }
    }

    SnapshotDelta {
        from_snapshot_id: prev.snapshot_id,
        to_snapshot_id: next.snapshot_id,
        added,
        removed,
        changed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: &str) -> SnapshotNode {
        SnapshotNode {
            id: id.to_string(),
            role: role.to_string(),
            name: None,
            value: None,
            states: Vec::new(),
            frame_id: None,
            bounds: None,
            stable_selector: None,
            actions: Vec::new(),
        }
    }

    #[test]
    fn diff_entries_identifies_add_remove_change() {
        let prev = build_cache_entry(1, vec![node("n1", "button"), node("n2", "heading")]);
        let next = build_cache_entry(2, vec![node("n1", "link"), node("n3", "textbox")]);
        let delta = diff_entries(&prev, &next);

        assert_eq!(delta.added.len(), 1);
        assert_eq!(delta.removed.len(), 1);
        assert_eq!(delta.changed.len(), 1);
    }
}
