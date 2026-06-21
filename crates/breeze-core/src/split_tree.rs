//! Binary-split-tree algebra: layout, divider collection, ratio updates,
//! insert/remove/swap/remap, and queries. No OS/UI dependency — geometry is the
//! toolkit-independent [`Rect`] (bottom-left origin).
//!
//! serde derives give the on-disk saved-session JSON format (parity-tested
//! against `tests/splitnode_fixture.json`).

use crate::geom::Rect;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Stable identity for a pane. Serializes as `{"id": N}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PaneID {
    pub id: i64,
}

impl PaneID {
    pub fn new(id: i64) -> PaneID {
        PaneID { id }
    }
}

/// Orientation of a split. Serializes as `"horizontal"` / `"vertical"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SplitDirection {
    /// children stacked top/bottom
    Horizontal,
    /// children side by side left/right
    Vertical,
}

/// A node in the split tree: either a single pane or a split of two subtrees.
///
/// `Leaf` is a struct variant with its field named `paneID` so the
/// internally-tagged JSON is `{"type":"leaf","paneID":{"id":N}}` and
/// `{"type":"split","direction":…,"ratio":…,"first":…,"second":…}` — the
/// saved-session on-disk format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SplitNode {
    #[serde(rename = "leaf")]
    Leaf {
        #[serde(rename = "paneID")]
        pane: PaneID,
    },
    #[serde(rename = "split")]
    Split {
        direction: SplitDirection,
        ratio: f64,
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

impl SplitNode {
    /// Convenience leaf constructor.
    pub fn leaf(pane: PaneID) -> SplitNode {
        SplitNode::Leaf { pane }
    }

    /// Convenience split constructor.
    pub fn split(direction: SplitDirection, ratio: f64, first: SplitNode, second: SplitNode) -> SplitNode {
        SplitNode::Split { direction, ratio, first: Box::new(first), second: Box::new(second) }
    }
}

/// Descriptor for one divider between two split children (for hit testing and
/// dragging).
#[derive(Debug, Clone, PartialEq)]
pub struct DividerInfo {
    pub direction: SplitDirection,
    /// bounding rect of the split node
    pub rect: Rect,
    /// absolute x (vertical) or y (horizontal)
    pub position: f64,
    /// navigation from root: true = first child, false = second
    pub path: Vec<bool>,
}

// Tree layout

/// Walk the tree, assigning a frame to each leaf via `assign`.
pub fn layout<F: FnMut(PaneID, Rect)>(node: &SplitNode, rect: Rect, assign: &mut F) {
    match node {
        SplitNode::Leaf { pane } => assign(*pane, rect),
        SplitNode::Split { direction, ratio, first, second } => {
            let (first_rect, second_rect) = split_rect(rect, *direction, *ratio);
            layout(first, first_rect, assign);
            layout(second, second_rect, assign);
        }
    }
}

/// Divide `rect` into two by `direction` and `ratio`. With a bottom-left
/// origin, the `first` child is the top portion of a horizontal split.
fn split_rect(rect: Rect, direction: SplitDirection, ratio: f64) -> (Rect, Rect) {
    match direction {
        SplitDirection::Vertical => {
            let left_w = rect.width * ratio;
            let left = Rect::new(rect.min_x(), rect.min_y(), left_w, rect.height);
            let right = Rect::new(rect.min_x() + left_w, rect.min_y(), rect.width - left_w, rect.height);
            (left, right)
        }
        SplitDirection::Horizontal => {
            // Top-left origin (y grows down): first = top portion at min_y.
            let top_h = rect.height * ratio;
            let top = Rect::new(rect.min_x(), rect.min_y(), rect.width, top_h);
            let bottom = Rect::new(rect.min_x(), rect.min_y() + top_h, rect.width, rect.height - top_h);
            (top, bottom)
        }
    }
}

// Divider collection

/// Collect a [`DividerInfo`] for every split in the tree, with the navigation
/// path to each.
pub fn collect_dividers(node: &SplitNode, rect: Rect, path: Vec<bool>, result: &mut Vec<DividerInfo>) {
    let SplitNode::Split { direction, ratio, first, second } = node else { return };

    let (first_rect, second_rect) = split_rect(rect, *direction, *ratio);

    let position = match direction {
        SplitDirection::Vertical => rect.min_x() + rect.width * *ratio,
        SplitDirection::Horizontal => rect.min_y() + rect.height * *ratio,
    };
    result.push(DividerInfo { direction: *direction, rect, position, path: path.clone() });

    let mut first_path = path.clone();
    first_path.push(true);
    collect_dividers(first, first_rect, first_path, result);

    let mut second_path = path;
    second_path.push(false);
    collect_dividers(second, second_rect, second_path, result);
}

// Ratio update

/// Navigate to a split by `path` and adjust its ratio by `delta` (clamped to
/// `[0.05, 0.95]`). Returns a new tree.
pub fn update_ratio(node: &SplitNode, path: &[bool], delta: f64, container_size: f64) -> SplitNode {
    let SplitNode::Split { direction, ratio, first, second } = node else { return node.clone() };

    if path.is_empty() {
        let min_ratio = 0.05;
        let delta_ratio = delta / container_size;
        // clamp to [min_ratio, 1 - min_ratio]
        let new_ratio = (*ratio + delta_ratio).min(1.0 - min_ratio).max(min_ratio);
        return SplitNode::Split {
            direction: *direction,
            ratio: new_ratio,
            first: first.clone(),
            second: second.clone(),
        };
    }

    if path[0] {
        let new_first = update_ratio(first, &path[1..], delta, container_size);
        SplitNode::Split {
            direction: *direction,
            ratio: *ratio,
            first: Box::new(new_first),
            second: second.clone(),
        }
    } else {
        let new_second = update_ratio(second, &path[1..], delta, container_size);
        SplitNode::Split {
            direction: *direction,
            ratio: *ratio,
            first: first.clone(),
            second: Box::new(new_second),
        }
    }
}

// Insert / remove

/// Split the leaf matching `target_id` into a 50/50 split holding the old leaf
/// plus `new_id`.
///
/// Note: the recursion forwards the outer `direction` argument, while a rebuilt
/// split keeps its own existing `dir` — so only the target leaf's new split
/// uses `direction`.
pub fn insert_leaf(new_id: PaneID, target_id: PaneID, direction: SplitDirection, node: &SplitNode) -> SplitNode {
    match node {
        SplitNode::Leaf { pane } => {
            if *pane == target_id {
                SplitNode::split(direction, 0.5, SplitNode::leaf(*pane), SplitNode::leaf(new_id))
            } else {
                node.clone()
            }
        }
        SplitNode::Split { direction: dir, ratio, first, second } => {
            let new_first = insert_leaf(new_id, target_id, direction, first);
            let new_second = insert_leaf(new_id, target_id, direction, second);
            SplitNode::split(*dir, *ratio, new_first, new_second)
        }
    }
}

/// Remove the leaf matching `target_id`, promoting its sibling into the parent
/// split's place. Returns `None` if the tree was a single matching leaf.
pub fn remove_leaf(target_id: PaneID, node: &SplitNode) -> Option<SplitNode> {
    match node {
        SplitNode::Leaf { pane } => {
            if *pane == target_id {
                None
            } else {
                Some(node.clone())
            }
        }
        SplitNode::Split { direction: dir, ratio, first, second } => {
            // Check if either direct child is the target leaf.
            if let SplitNode::Leaf { pane: f_id } = first.as_ref() {
                if *f_id == target_id {
                    return Some((**second).clone());
                }
            }
            if let SplitNode::Leaf { pane: s_id } = second.as_ref() {
                if *s_id == target_id {
                    return Some((**first).clone());
                }
            }
            // Recurse into children.
            if let Some(new_first) = remove_leaf(target_id, first) {
                if let Some(new_second) = remove_leaf(target_id, second) {
                    return Some(SplitNode::split(*dir, *ratio, new_first, new_second));
                }
                return Some(new_first);
            }
            if let Some(new_second) = remove_leaf(target_id, second) {
                return Some(new_second);
            }
            None
        }
    }
}

/// Exchange the positions of two pane IDs in the tree.
pub fn swap_leaves(a: PaneID, b: PaneID, node: &SplitNode) -> SplitNode {
    match node {
        SplitNode::Leaf { pane } => {
            if *pane == a {
                SplitNode::leaf(b)
            } else if *pane == b {
                SplitNode::leaf(a)
            } else {
                node.clone()
            }
        }
        SplitNode::Split { direction, ratio, first, second } => {
            SplitNode::split(*direction, *ratio, swap_leaves(a, b, first), swap_leaves(a, b, second))
        }
    }
}

/// Replace every leaf ID via `map` in a single pass — won't alias when the old
/// and new ID spaces overlap (used to restore a saved tree onto freshly
/// re-created panes).
pub fn remap_leaves(node: &SplitNode, map: &HashMap<PaneID, PaneID>) -> SplitNode {
    match node {
        SplitNode::Leaf { pane } => SplitNode::leaf(*map.get(pane).unwrap_or(pane)),
        SplitNode::Split { direction, ratio, first, second } => {
            SplitNode::split(*direction, *ratio, remap_leaves(first, map), remap_leaves(second, map))
        }
    }
}

// Queries

/// All leaf pane IDs in the tree.
pub fn collect_leaf_ids(node: Option<&SplitNode>) -> HashSet<PaneID> {
    let Some(node) = node else { return HashSet::new() };
    match node {
        SplitNode::Leaf { pane } => {
            let mut set = HashSet::new();
            set.insert(*pane);
            set
        }
        SplitNode::Split { first, second, .. } => {
            let mut set = collect_leaf_ids(Some(first));
            set.extend(collect_leaf_ids(Some(second)));
            set
        }
    }
}

/// The least-nested leaf (ties resolve to the left subtree).
pub fn shallowest_leaf(node: &SplitNode, current_depth: i32) -> (PaneID, i32) {
    match node {
        SplitNode::Leaf { pane } => (*pane, current_depth),
        SplitNode::Split { first, second, .. } => {
            let left = shallowest_leaf(first, current_depth + 1);
            let right = shallowest_leaf(second, current_depth + 1);
            if left.1 <= right.1 {
                left
            } else {
                right
            }
        }
    }
}

/// Depth of a leaf in the tree, or `-1` if not present.
pub fn depth_of(id: PaneID, node: &SplitNode, current_depth: i32) -> i32 {
    match node {
        SplitNode::Leaf { pane } => {
            if *pane == id {
                current_depth
            } else {
                -1
            }
        }
        SplitNode::Split { first, second, .. } => {
            let d = depth_of(id, first, current_depth + 1);
            if d >= 0 {
                d
            } else {
                depth_of(id, second, current_depth + 1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: i64) -> PaneID {
        PaneID::new(id)
    }

    /// Sample tree used across layout/JSON tests:
    /// vertical split (ratio 0.6): left = leaf 1, right = horizontal split
    /// (ratio 0.5) of leaf 2 (top) / leaf 3 (bottom).
    fn sample() -> SplitNode {
        SplitNode::split(
            SplitDirection::Vertical,
            0.6,
            SplitNode::leaf(p(1)),
            SplitNode::split(SplitDirection::Horizontal, 0.5, SplitNode::leaf(p(2)), SplitNode::leaf(p(3))),
        )
    }

    #[test]
    fn layout_leaf_gets_full_rect() {
        let mut got = Vec::new();
        layout(&SplitNode::leaf(p(7)), Rect::new(0.0, 0.0, 100.0, 50.0), &mut |id, r| got.push((id, r)));
        assert_eq!(got, vec![(p(7), Rect::new(0.0, 0.0, 100.0, 50.0))]);
    }

    #[test]
    fn layout_vertical_splits_width_by_ratio() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.25, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let mut got = std::collections::HashMap::new();
        layout(&tree, Rect::new(0.0, 0.0, 200.0, 100.0), &mut |id, r| {
            got.insert(id, r);
        });
        assert_eq!(got[&p(1)], Rect::new(0.0, 0.0, 50.0, 100.0)); // 200*0.25
        assert_eq!(got[&p(2)], Rect::new(50.0, 0.0, 150.0, 100.0));
    }

    #[test]
    fn layout_horizontal_top_first_top_left_origin() {
        let tree = SplitNode::split(SplitDirection::Horizontal, 0.25, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let mut got = std::collections::HashMap::new();
        layout(&tree, Rect::new(0.0, 0.0, 200.0, 100.0), &mut |id, r| {
            got.insert(id, r);
        });
        // Top-left origin: topH = 100*0.25 = 25; first (top) sits at y = 0.
        assert_eq!(got[&p(1)], Rect::new(0.0, 0.0, 200.0, 25.0));
        assert_eq!(got[&p(2)], Rect::new(0.0, 25.0, 200.0, 75.0));
    }

    #[test]
    fn collect_dividers_count_path_position() {
        let mut out = Vec::new();
        collect_dividers(&sample(), Rect::new(0.0, 0.0, 100.0, 100.0), Vec::new(), &mut out);
        assert_eq!(out.len(), 2);
        // root vertical divider: position = minX + w*ratio = 0 + 100*0.6 = 60, path [].
        assert_eq!(out[0].direction, SplitDirection::Vertical);
        assert_eq!(out[0].position, 60.0);
        assert!(out[0].path.is_empty());
        // nested horizontal divider lives in the right rect [60,0,40,100], path [false].
        // position = minY + h*(1-ratio) = 0 + 100*0.5 = 50.
        assert_eq!(out[1].direction, SplitDirection::Horizontal);
        assert_eq!(out[1].position, 50.0);
        assert_eq!(out[1].path, vec![false]);
    }

    #[test]
    fn update_ratio_root_and_clamps() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        // +20 over container 100 → +0.2 → 0.7
        let up = update_ratio(&tree, &[], 20.0, 100.0);
        if let SplitNode::Split { ratio, .. } = up {
            assert!((ratio - 0.7).abs() < 1e-9);
        } else {
            panic!("expected split");
        }
        // huge positive delta clamps to 0.95
        let hi = update_ratio(&tree, &[], 1000.0, 100.0);
        if let SplitNode::Split { ratio, .. } = hi {
            assert!((ratio - 0.95).abs() < 1e-9);
        } else {
            panic!();
        }
        // huge negative delta clamps to 0.05
        let lo = update_ratio(&tree, &[], -1000.0, 100.0);
        if let SplitNode::Split { ratio, .. } = lo {
            assert!((ratio - 0.05).abs() < 1e-9);
        } else {
            panic!();
        }
    }

    #[test]
    fn update_ratio_nested_path() {
        let up = update_ratio(&sample(), &[false], 10.0, 100.0); // nested horizontal: 0.5 + 0.1 = 0.6
        if let SplitNode::Split { second, .. } = up {
            if let SplitNode::Split { ratio, .. } = *second {
                assert!((ratio - 0.6).abs() < 1e-9);
            } else {
                panic!("expected nested split");
            }
        } else {
            panic!();
        }
    }

    #[test]
    fn insert_leaf_splits_target_with_outer_direction() {
        let got = insert_leaf(p(9), p(2), SplitDirection::Vertical, &sample());
        // leaf 2 should become a vertical split of [2, 9] (outer direction used).
        let mut leaves = Vec::new();
        layout(&got, Rect::new(0.0, 0.0, 100.0, 100.0), &mut |id, _| leaves.push(id));
        assert!(leaves.contains(&p(9)));
        assert_eq!(collect_leaf_ids(Some(&got)).len(), 4);
    }

    #[test]
    fn remove_leaf_promotes_sibling() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        assert_eq!(remove_leaf(p(2), &tree), Some(SplitNode::leaf(p(1))));
        // remove only node → None
        assert_eq!(remove_leaf(p(5), &SplitNode::leaf(p(5))), None);
        // nested removal collapses the inner split.
        let got = remove_leaf(p(3), &sample()).unwrap();
        let ids = collect_leaf_ids(Some(&got));
        assert_eq!(ids, [p(1), p(2)].into_iter().collect());
    }

    #[test]
    fn swap_leaves_exchanges_ids() {
        let got = swap_leaves(p(1), p(3), &sample());
        assert_eq!(depth_of(p(1), &got, 0), 2); // 1 now where 3 was (nested)
        assert_eq!(depth_of(p(3), &got, 0), 1); // 3 now where 1 was (root-left)
    }

    #[test]
    fn remap_leaves_no_alias_on_overlap() {
        // 1->2, 2->1 simultaneously; chaining swaps would alias, single pass must not.
        let map: HashMap<PaneID, PaneID> = [(p(1), p(2)), (p(2), p(1))].into_iter().collect();
        let got = remap_leaves(&sample(), &map);
        let mut leaves = Vec::new();
        layout(&got, Rect::new(0.0, 0.0, 100.0, 100.0), &mut |id, _| leaves.push(id));
        // 1 and 2 cleanly swapped, 3 untouched.
        assert_eq!(depth_of(p(2), &got, 0), 1); // was leaf 1 at root-left
        assert_eq!(depth_of(p(1), &got, 0), 2); // was leaf 2 nested
        assert_eq!(depth_of(p(3), &got, 0), 2);
    }

    #[test]
    fn collect_leaf_ids_all_and_none() {
        assert!(collect_leaf_ids(None).is_empty());
        assert_eq!(collect_leaf_ids(Some(&sample())), [p(1), p(2), p(3)].into_iter().collect());
    }

    #[test]
    fn shallowest_leaf_prefers_left_on_tie() {
        // root-left leaf 1 has depth 1; the nested leaves have depth 2.
        assert_eq!(shallowest_leaf(&sample(), 0), (p(1), 1));
    }

    #[test]
    fn depth_of_present_and_absent() {
        assert_eq!(depth_of(p(1), &sample(), 0), 1);
        assert_eq!(depth_of(p(3), &sample(), 0), 2);
        assert_eq!(depth_of(p(99), &sample(), 0), -1);
    }

    #[test]
    fn json_round_trip() {
        let tree = sample();
        let json = serde_json::to_string(&tree).unwrap();
        let back: SplitNode = serde_json::from_str(&json).unwrap();
        assert_eq!(tree, back);
    }

    #[test]
    fn json_field_names() {
        let v: serde_json::Value = serde_json::to_value(SplitNode::leaf(p(42))).unwrap();
        assert_eq!(v["type"], "leaf");
        assert_eq!(v["paneID"]["id"], 42);

        let s: serde_json::Value = serde_json::to_value(SplitNode::split(
            SplitDirection::Vertical,
            0.5,
            SplitNode::leaf(p(1)),
            SplitNode::leaf(p(2)),
        ))
        .unwrap();
        assert_eq!(s["type"], "split");
        assert_eq!(s["direction"], "vertical");
        assert_eq!(s["ratio"], 0.5);
        assert_eq!(s["first"]["paneID"]["id"], 1);
        assert_eq!(s["second"]["paneID"]["id"], 2);
    }

    #[test]
    fn decodes_saved_session_fixture() {
        // Reference saved-session JSON for the sample tree. Object key order is
        // not asserted (it's unspecified) — decode and compare structurally.
        let fixture = include_str!("../tests/splitnode_fixture.json");
        let decoded: SplitNode = serde_json::from_str(fixture).expect("decode fixture");
        assert_eq!(decoded, sample());
    }

    #[test]
    fn unknown_type_fails_to_decode() {
        let bad = r#"{"type":"triple","paneID":{"id":1}}"#;
        assert!(serde_json::from_str::<SplitNode>(bad).is_err());
    }
}
