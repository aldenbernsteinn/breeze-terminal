//! Pane geometry + focus order over the split tree. The window uses these to
//! place each pane's terminal grid and to cycle focus; the tree ops themselves
//! live in `breeze_core::split_tree`.

use breeze_core::geom::Rect;
use breeze_core::split_tree::{collect_dividers, layout, DividerInfo, PaneID, SplitDirection, SplitNode};

/// The on-screen rectangle for every leaf pane, given the content area.
pub fn pane_rects(node: &SplitNode, rect: Rect) -> Vec<(PaneID, Rect)> {
    let mut out = Vec::new();
    layout(node, rect, &mut |id, r| out.push((id, r)));
    out
}

/// The pane whose rectangle contains the point (`px`,`py`), if any.
pub fn pane_at(node: &SplitNode, rect: Rect, px: f64, py: f64) -> Option<PaneID> {
    pane_rects(node, rect).into_iter().find_map(|(id, r)| {
        if px >= r.x && px < r.x + r.width && py >= r.y && py < r.y + r.height {
            Some(id)
        } else {
            None
        }
    })
}

/// The divider near point (`px`,`py`) within `tol` pixels of its line, if any
/// (deepest match wins). Used to start a drag-to-resize.
pub fn divider_at(node: &SplitNode, rect: Rect, px: f64, py: f64, tol: f64) -> Option<DividerInfo> {
    let mut dividers = Vec::new();
    collect_dividers(node, rect, Vec::new(), &mut dividers);
    // Reverse so deeper (later-collected) dividers take precedence on overlap.
    dividers.into_iter().rev().find(|d| {
        let r = d.rect;
        match d.direction {
            SplitDirection::Vertical => {
                (px - d.position).abs() <= tol && py >= r.y && py <= r.y + r.height
            }
            SplitDirection::Horizontal => {
                (py - d.position).abs() <= tol && px >= r.x && px <= r.x + r.width
            }
        }
    })
}

/// Every divider whose line passes within `tol` of the point (`px`,`py`) — used
/// to detect an intersection grab (one vertical + one horizontal) for omni drag.
pub fn dividers_at(node: &SplitNode, rect: Rect, px: f64, py: f64, tol: f64) -> Vec<DividerInfo> {
    all_dividers(node, rect)
        .into_iter()
        .filter(|d| {
            let r = d.rect;
            match d.direction {
                SplitDirection::Vertical => (px - d.position).abs() <= tol && py >= r.y && py <= r.y + r.height,
                SplitDirection::Horizontal => (py - d.position).abs() <= tol && px >= r.x && px <= r.x + r.width,
            }
        })
        .collect()
}

/// Every divider in the tree, with its on-screen position and path.
pub fn all_dividers(node: &SplitNode, rect: Rect) -> Vec<DividerInfo> {
    let mut dividers = Vec::new();
    collect_dividers(node, rect, Vec::new(), &mut dividers);
    dividers
}

/// Paths of all dividers aligned with `grabbed` — same orientation and the same
/// on-screen position within `tol` — so dragging one moves them together
/// (linked drag across split boundaries). Includes `grabbed` itself.
pub fn find_linked(dividers: &[DividerInfo], grabbed: &DividerInfo, tol: f64) -> Vec<Vec<bool>> {
    dividers
        .iter()
        .filter(|d| d.direction == grabbed.direction && (d.position - grabbed.position).abs() <= tol)
        .map(|d| d.path.clone())
        .collect()
}

/// Leaf pane ids in left-to-right / top-to-bottom traversal order.
pub fn leaf_ids_in_order(node: &SplitNode) -> Vec<PaneID> {
    fn rec(n: &SplitNode, out: &mut Vec<PaneID>) {
        match n {
            SplitNode::Leaf { pane } => out.push(*pane),
            SplitNode::Split { first, second, .. } => {
                rec(first, out);
                rec(second, out);
            }
        }
    }
    let mut out = Vec::new();
    rec(node, &mut out);
    out
}

/// The pane to focus when cycling from `current` (`forward` = next, else prev),
/// wrapping. `None` only if the tree has no leaves.
pub fn focus_after(node: &SplitNode, current: PaneID, forward: bool) -> Option<PaneID> {
    let ids = leaf_ids_in_order(node);
    if ids.is_empty() {
        return None;
    }
    let pos = ids.iter().position(|&p| p == current).unwrap_or(0);
    let n = ids.len();
    let next = if forward { (pos + 1) % n } else { (pos + n - 1) % n };
    Some(ids[next])
}

#[cfg(test)]
mod tests {
    use super::*;
    use breeze_core::split_tree::{SplitDirection, SplitNode};

    fn p(id: i64) -> PaneID {
        PaneID::new(id)
    }

    #[test]
    fn single_leaf_fills_the_area() {
        let tree = SplitNode::leaf(p(1));
        let rects = pane_rects(&tree, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(rects, vec![(p(1), Rect::new(0.0, 0.0, 800.0, 600.0))]);
    }

    #[test]
    fn linked_dividers_share_orientation_and_position() {
        // Horizontal split whose two halves each split vertically at 0.5: the
        // two vertical dividers line up at x=400 and should drag together.
        let half = || SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let tree = SplitNode::split(SplitDirection::Horizontal, 0.5, half(), half());
        let rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        let dividers = all_dividers(&tree, rect);
        // One horizontal (top-level) + two aligned verticals.
        let vert: Vec<_> = dividers.iter().filter(|d| d.direction == SplitDirection::Vertical).collect();
        assert_eq!(vert.len(), 2);
        let linked = find_linked(&dividers, vert[0], 2.0);
        assert_eq!(linked.len(), 2, "both aligned vertical dividers should link");
        // The lone horizontal divider links only to itself.
        let horiz = dividers.iter().find(|d| d.direction == SplitDirection::Horizontal).unwrap();
        assert_eq!(find_linked(&dividers, horiz, 2.0).len(), 1);
    }

    #[test]
    fn dividers_at_intersection_returns_both_axes() {
        let half = || SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let tree = SplitNode::split(SplitDirection::Horizontal, 0.5, half(), half());
        let rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        // The center (400, 300) is where the horizontal divider crosses the
        // aligned vertical dividers — an omni-drag grab.
        let near = dividers_at(&tree, rect, 400.0, 300.0, 3.0);
        assert!(near.iter().any(|d| d.direction == SplitDirection::Vertical));
        assert!(near.iter().any(|d| d.direction == SplitDirection::Horizontal));
        // A point along just the horizontal line (away from x=400) hits only it.
        let h_only = dividers_at(&tree, rect, 100.0, 300.0, 3.0);
        assert!(h_only.iter().all(|d| d.direction == SplitDirection::Horizontal));
    }

    #[test]
    fn vertical_split_divides_width() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let rects = pane_rects(&tree, Rect::new(0.0, 0.0, 800.0, 600.0));
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0].1.width, 400.0);
        assert_eq!(rects[1].1.x, 400.0);
    }

    #[test]
    fn divider_at_finds_the_split_line() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let area = Rect::new(0.0, 0.0, 800.0, 600.0);
        // Vertical divider sits at x = 400.
        let d = divider_at(&tree, area, 402.0, 300.0, 6.0).expect("divider near x=400");
        assert_eq!(d.direction, SplitDirection::Vertical);
        assert!(d.path.is_empty());
        assert!(divider_at(&tree, area, 200.0, 300.0, 6.0).is_none());
    }

    #[test]
    fn pane_at_hit_tests_rectangles() {
        let tree = SplitNode::split(SplitDirection::Vertical, 0.5, SplitNode::leaf(p(1)), SplitNode::leaf(p(2)));
        let area = Rect::new(0.0, 0.0, 800.0, 600.0);
        assert_eq!(pane_at(&tree, area, 100.0, 100.0), Some(p(1)));
        assert_eq!(pane_at(&tree, area, 500.0, 100.0), Some(p(2)));
        assert_eq!(pane_at(&tree, area, 900.0, 100.0), None); // outside
    }

    #[test]
    fn focus_cycles_in_order_and_wraps() {
        let tree = SplitNode::split(
            SplitDirection::Vertical,
            0.5,
            SplitNode::leaf(p(1)),
            SplitNode::split(SplitDirection::Horizontal, 0.5, SplitNode::leaf(p(2)), SplitNode::leaf(p(3))),
        );
        assert_eq!(leaf_ids_in_order(&tree), vec![p(1), p(2), p(3)]);
        assert_eq!(focus_after(&tree, p(1), true), Some(p(2)));
        assert_eq!(focus_after(&tree, p(3), true), Some(p(1))); // wrap
        assert_eq!(focus_after(&tree, p(1), false), Some(p(3))); // wrap back
    }
}
