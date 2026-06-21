//! A window's pane workspace: the split-tree layout, the focused pane, and id
//! allocation. Splitting/closing/focus-cycling is expressed here over the
//! ported tree ops; the window binds a terminal to each pane id.

use crate::panes::focus_after;
use breeze_core::split_tree::{insert_leaf, remove_leaf, update_ratio, PaneID, SplitDirection, SplitNode};

pub struct Workspace {
    tree: SplitNode,
    focused: PaneID,
    next_id: i64,
}

impl Default for Workspace {
    fn default() -> Self {
        Workspace::new()
    }
}

impl Workspace {
    /// Start with a single pane (id 0).
    pub fn new() -> Workspace {
        Workspace { tree: SplitNode::leaf(PaneID::new(0)), focused: PaneID::new(0), next_id: 1 }
    }

    /// Rebuild from a restored split tree: focus the first leaf, allocate new
    /// ids above the highest existing one.
    pub fn from_tree(tree: SplitNode) -> Workspace {
        let leaves = crate::panes::leaf_ids_in_order(&tree);
        let focused = leaves.first().copied().unwrap_or(PaneID::new(0));
        let next_id = leaves.iter().map(|p| p.id).max().unwrap_or(-1) + 1;
        Workspace { tree, focused, next_id }
    }

    pub fn tree(&self) -> &SplitNode {
        &self.tree
    }

    pub fn focused(&self) -> PaneID {
        self.focused
    }

    pub fn pane_count(&self) -> usize {
        crate::panes::leaf_ids_in_order(&self.tree).len()
    }

    /// Split the focused pane in `direction`, creating a new pane that becomes
    /// focused. Returns the new pane id.
    pub fn split(&mut self, direction: SplitDirection) -> PaneID {
        let new_id = PaneID::new(self.next_id);
        self.next_id += 1;
        self.tree = insert_leaf(new_id, self.focused, direction, &self.tree);
        self.focused = new_id;
        new_id
    }

    /// Add a pane in a balanced way: split the **shallowest** leaf, with the
    /// direction alternating by that leaf's depth (even → vertical, odd →
    /// horizontal). This grows the grid evenly instead of subdividing the
    /// focused corner. Returns the new pane id (which becomes focused).
    pub fn add_pane_balanced(&mut self) -> PaneID {
        let (target, depth) = breeze_core::split_tree::shallowest_leaf(&self.tree, 0);
        let dir =
            if depth % 2 == 0 { SplitDirection::Vertical } else { SplitDirection::Horizontal };
        let new_id = PaneID::new(self.next_id);
        self.next_id += 1;
        self.tree = insert_leaf(new_id, target, dir, &self.tree);
        self.focused = new_id;
        new_id
    }

    /// Close the focused pane; its sibling takes over. Returns the closed id, or
    /// `None` if it was the only pane (closing the last pane is the window's
    /// call, not ours).
    pub fn close_focused(&mut self) -> Option<PaneID> {
        let closed = self.focused;
        match remove_leaf(closed, &self.tree) {
            Some(new_tree) => {
                self.tree = new_tree;
                // Focus the first remaining leaf.
                if let Some(first) = crate::panes::leaf_ids_in_order(&self.tree).first().copied() {
                    self.focused = first;
                }
                Some(closed)
            }
            None => None, // last pane — caller decides whether to close the window
        }
    }

    /// Close an arbitrary pane by id. Returns the closed id, or `None` if it was
    /// the only pane. If the focused pane was the one removed, focus shifts to
    /// the first remaining leaf.
    pub fn close(&mut self, id: PaneID) -> Option<PaneID> {
        match remove_leaf(id, &self.tree) {
            Some(new_tree) => {
                self.tree = new_tree;
                if self.focused == id {
                    if let Some(first) = crate::panes::leaf_ids_in_order(&self.tree).first().copied() {
                        self.focused = first;
                    }
                }
                Some(id)
            }
            None => None,
        }
    }

    /// Move focus to the next/previous pane, wrapping.
    pub fn focus_cycle(&mut self, forward: bool) {
        if let Some(next) = focus_after(&self.tree, self.focused, forward) {
            self.focused = next;
        }
    }

    /// Adjust the split ratio at `path` by `delta` pixels over `container`
    /// (drag-to-resize a divider).
    pub fn resize_divider(&mut self, path: &[bool], delta: f64, container: f64) {
        self.tree = update_ratio(&self.tree, path, delta, container);
    }

    /// Swap the focused pane with the next pane in order (wrapping). The two
    /// bound terminals trade screen positions; focus stays on the same terminal.
    /// No-op with fewer than two panes.
    pub fn swap_focused_with_next(&mut self) {
        let leaves = crate::panes::leaf_ids_in_order(&self.tree);
        if leaves.len() < 2 {
            return;
        }
        let i = leaves.iter().position(|p| *p == self.focused).unwrap_or(0);
        let other = leaves[(i + 1) % leaves.len()];
        self.tree = breeze_core::split_tree::swap_leaves(self.focused, other, &self.tree);
    }

    /// Swap two panes' positions (drag-to-move). No-op if either is absent.
    pub fn swap(&mut self, a: PaneID, b: PaneID) {
        if a == b {
            return;
        }
        let leaves = crate::panes::leaf_ids_in_order(&self.tree);
        if leaves.contains(&a) && leaves.contains(&b) {
            self.tree = breeze_core::split_tree::swap_leaves(a, b, &self.tree);
        }
    }

    /// Explicitly focus a pane (e.g. on click). No-op if it isn't a leaf.
    pub fn focus(&mut self, id: PaneID) {
        if crate::panes::leaf_ids_in_order(&self.tree).contains(&id) {
            self.focused = id;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_with_one_focused_pane() {
        let ws = Workspace::new();
        assert_eq!(ws.pane_count(), 1);
        assert_eq!(ws.focused(), PaneID::new(0));
    }

    #[test]
    fn split_adds_and_focuses_new_pane() {
        let mut ws = Workspace::new();
        let a = ws.split(SplitDirection::Vertical);
        assert_eq!(ws.pane_count(), 2);
        assert_eq!(ws.focused(), a);
        let b = ws.split(SplitDirection::Horizontal);
        assert_eq!(ws.pane_count(), 3);
        assert_eq!(ws.focused(), b);
    }

    #[test]
    fn close_collapses_and_keeps_a_focus() {
        let mut ws = Workspace::new();
        ws.split(SplitDirection::Vertical);
        ws.split(SplitDirection::Horizontal);
        assert_eq!(ws.pane_count(), 3);
        let closed = ws.close_focused();
        assert!(closed.is_some());
        assert_eq!(ws.pane_count(), 2);
        // Focus is a still-living leaf.
        assert!(crate::panes::leaf_ids_in_order(ws.tree()).contains(&ws.focused()));
    }

    #[test]
    fn swap_moves_focused_terminal_to_next_position() {
        let mut ws = Workspace::new(); // pane 0
        let a = ws.split(SplitDirection::Vertical); // pane 1, focused, on the right
        // Order is [0, 1]; focused is 1 (last). Swapping with "next" wraps to 0.
        let before = crate::panes::leaf_ids_in_order(ws.tree());
        assert_eq!(before, vec![PaneID::new(0), a]);
        ws.swap_focused_with_next();
        let after = crate::panes::leaf_ids_in_order(ws.tree());
        // Positions traded: pane 1 now sits first, pane 0 second.
        assert_eq!(after, vec![a, PaneID::new(0)]);
        // Focus still tracks the same terminal.
        assert_eq!(ws.focused(), a);
    }

    #[test]
    fn close_arbitrary_pane_keeps_others() {
        let mut ws = Workspace::new(); // pane 0
        let a = ws.split(SplitDirection::Vertical); // pane 1 (focused)
        let b = ws.split(SplitDirection::Horizontal); // pane 2 (focused)
        assert_eq!(ws.pane_count(), 3);
        // Close pane 0 (not focused) — others remain, focus unchanged.
        assert_eq!(ws.close(PaneID::new(0)), Some(PaneID::new(0)));
        assert_eq!(ws.pane_count(), 2);
        assert_eq!(ws.focused(), b);
        let leaves = crate::panes::leaf_ids_in_order(ws.tree());
        assert!(leaves.contains(&a) && leaves.contains(&b));
        // Closing the focused pane refocuses a survivor.
        ws.close(b);
        assert_eq!(ws.focused(), a);
        // Closing the last pane returns None.
        assert_eq!(ws.close(a), None);
    }

    #[test]
    fn swap_with_single_pane_is_noop() {
        let mut ws = Workspace::new();
        ws.swap_focused_with_next();
        assert_eq!(ws.pane_count(), 1);
    }

    #[test]
    fn swap_two_panes_trades_positions() {
        let mut ws = Workspace::new(); // pane 0
        let a = ws.split(SplitDirection::Vertical); // pane 1
        let before = crate::panes::leaf_ids_in_order(ws.tree());
        ws.swap(PaneID::new(0), a);
        let after = crate::panes::leaf_ids_in_order(ws.tree());
        assert_eq!(after, vec![before[1], before[0]]);
    }

    #[test]
    fn resize_divider_changes_ratio() {
        use breeze_core::geom::Rect;
        let mut ws = Workspace::new();
        ws.split(SplitDirection::Vertical); // 50/50 of the root
        ws.resize_divider(&[], 80.0, 800.0); // +0.1 → 0.6
        let rects = crate::panes::pane_rects(ws.tree(), Rect::new(0.0, 0.0, 800.0, 600.0));
        // First pane should now be ~480 wide (0.6 * 800).
        assert!((rects[0].1.width - 480.0).abs() < 1.0, "got {}", rects[0].1.width);
    }

    #[test]
    fn cannot_close_last_pane() {
        let mut ws = Workspace::new();
        assert_eq!(ws.close_focused(), None);
        assert_eq!(ws.pane_count(), 1);
    }

    #[test]
    fn from_tree_focuses_first_and_allocates_above_max() {
        use breeze_core::split_tree::{PaneID, SplitNode};
        let tree = SplitNode::split(
            SplitDirection::Vertical,
            0.5,
            SplitNode::leaf(PaneID::new(3)),
            SplitNode::leaf(PaneID::new(7)),
        );
        let mut ws = Workspace::from_tree(tree);
        assert_eq!(ws.pane_count(), 2);
        assert_eq!(ws.focused(), PaneID::new(3));
        let new = ws.split(SplitDirection::Vertical);
        assert_eq!(new, PaneID::new(8)); // above the max (7)
    }

    #[test]
    fn focus_cycle_wraps() {
        let mut ws = Workspace::new();
        let a = ws.split(SplitDirection::Vertical); // panes: 0, a; focused a
        ws.focus_cycle(true);
        assert_eq!(ws.focused(), PaneID::new(0)); // wrapped past a back to 0
        ws.focus_cycle(false);
        assert_eq!(ws.focused(), a);
    }
}
