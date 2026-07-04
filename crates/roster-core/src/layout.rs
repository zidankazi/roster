//! The split tree and the math that turns it into pane rectangles.
//!
//! A window's layout is a binary tree: leaves are panes, interior nodes are
//! splits with a direction and a ratio. [`layout`] walks the tree and tiles a
//! target rectangle exactly — no gaps, no overlap. Separators, if any, are a
//! rendering concern and are drawn inside pane rects by `roster-tui`.

use crate::session::PaneId;

/// A rectangle in terminal cell coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    /// Left column.
    pub x: u16,
    /// Top row.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

impl Rect {
    /// A rect at (`x`, `y`) with the given size.
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
}

/// How a split arranges its two children.
///
/// Follows ratatui's `Direction` convention: `Horizontal` lays children out
/// along the horizontal axis (side by side, divider vertical); `Vertical`
/// stacks them (one above the other, divider horizontal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDirection {
    /// Children side by side.
    Horizontal,
    /// Children stacked top to bottom.
    Vertical,
}

/// A node in a window's split tree.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum LayoutNode {
    Leaf(PaneId),
    Split {
        direction: SplitDirection,
        /// Fraction of the axis given to `first`, in (0, 1).
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

impl LayoutNode {
    /// Replace the leaf holding `target` with a split of `target` and `new`.
    /// Returns `true` if the target was found.
    pub(crate) fn split_leaf(
        &mut self,
        target: PaneId,
        new: PaneId,
        direction: SplitDirection,
    ) -> bool {
        match self {
            LayoutNode::Leaf(id) if *id == target => {
                *self = LayoutNode::Split {
                    direction,
                    ratio: 0.5,
                    first: Box::new(LayoutNode::Leaf(target)),
                    second: Box::new(LayoutNode::Leaf(new)),
                };
                true
            }
            LayoutNode::Leaf(_) => false,
            LayoutNode::Split { first, second, .. } => {
                first.split_leaf(target, new, direction)
                    || second.split_leaf(target, new, direction)
            }
        }
    }

    /// Remove the leaf holding `target`, collapsing its parent split into the
    /// sibling. Returns `RemoveOutcome::Removed` with the surviving subtree,
    /// `LastLeaf` if the tree is just this leaf, or `NotFound`.
    pub(crate) fn remove_leaf(self, target: PaneId) -> RemoveOutcome {
        match self {
            LayoutNode::Leaf(id) if id == target => RemoveOutcome::LastLeaf,
            leaf @ LayoutNode::Leaf(_) => RemoveOutcome::NotFound(leaf),
            LayoutNode::Split {
                direction,
                ratio,
                first,
                second,
            } => match first.remove_leaf(target) {
                RemoveOutcome::LastLeaf => RemoveOutcome::Removed(*second),
                RemoveOutcome::Removed(node) => RemoveOutcome::Removed(LayoutNode::Split {
                    direction,
                    ratio,
                    first: Box::new(node),
                    second,
                }),
                RemoveOutcome::NotFound(first) => match second.remove_leaf(target) {
                    RemoveOutcome::LastLeaf => RemoveOutcome::Removed(first),
                    RemoveOutcome::Removed(node) => RemoveOutcome::Removed(LayoutNode::Split {
                        direction,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(node),
                    }),
                    RemoveOutcome::NotFound(second) => RemoveOutcome::NotFound(LayoutNode::Split {
                        direction,
                        ratio,
                        first: Box::new(first),
                        second: Box::new(second),
                    }),
                },
            },
        }
    }

    /// Pane ids of all leaves, left-to-right / top-to-bottom tree order.
    pub(crate) fn leaves(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    /// The direction of the divider under (`x`, `y`) when this tree is laid
    /// out in `area`: the last column of a horizontal split's first half, or
    /// the first row of a vertical split's second half (where the lower
    /// pane's title bar sits). Deeper splits shadow shallower ones only in
    /// regions they own, so positions are unambiguous.
    pub(crate) fn divider_at(&self, area: Rect, x: u16, y: u16) -> Option<SplitDirection> {
        let LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, b) = divide(area, *direction, *ratio);
        match direction {
            SplitDirection::Horizontal
                if a.width > 0 && x == a.x + a.width - 1 && y >= a.y && y < a.y + a.height =>
            {
                Some(SplitDirection::Horizontal)
            }
            SplitDirection::Vertical if y == b.y && x >= b.x && x < b.x + b.width => {
                Some(SplitDirection::Vertical)
            }
            _ => first
                .divider_at(a, x, y)
                .or_else(|| second.divider_at(b, x, y)),
        }
    }

    /// Move the divider under (`from_x`, `from_y`) so it lands as close to
    /// `to` as the layout allows. Returns the divider's new position, or
    /// `None` when nothing draggable is there.
    pub(crate) fn drag_divider(
        &mut self,
        area: Rect,
        from: (u16, u16),
        to: (u16, u16),
    ) -> Option<(u16, u16)> {
        let LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };
        let (a, b) = divide(area, *direction, *ratio);
        let owned = match direction {
            SplitDirection::Horizontal => {
                a.width > 0
                    && from.0 == a.x + a.width - 1
                    && from.1 >= a.y
                    && from.1 < a.y + a.height
            }
            SplitDirection::Vertical => from.1 == b.y && from.0 >= b.x && from.0 < b.x + b.width,
        };
        if !owned {
            return first
                .drag_divider(a, from, to)
                .or_else(|| second.drag_divider(b, from, to));
        }
        match direction {
            SplitDirection::Horizontal if area.width >= 2 => {
                let target = (to.0.saturating_sub(area.x) + 1) as f32 / f32::from(area.width);
                *ratio = target.clamp(0.05, 0.95);
                let first_w = portion(area.width, *ratio);
                Some((area.x + first_w - 1, from.1))
            }
            SplitDirection::Vertical if area.height >= 2 => {
                let target = to.1.saturating_sub(area.y) as f32 / f32::from(area.height);
                *ratio = target.clamp(0.05, 0.95);
                let first_h = portion(area.height, *ratio);
                Some((from.0, area.y + first_h))
            }
            _ => None,
        }
    }

    fn collect_leaves(&self, out: &mut Vec<PaneId>) {
        match self {
            LayoutNode::Leaf(id) => out.push(*id),
            LayoutNode::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }
}

/// Outcome of [`LayoutNode::remove_leaf`].
pub(crate) enum RemoveOutcome {
    /// The leaf was removed; this is the surviving tree.
    Removed(LayoutNode),
    /// The tree consisted solely of the target leaf; nothing survives.
    LastLeaf,
    /// The target was not in this tree; the tree is returned unchanged.
    NotFound(LayoutNode),
}

/// Compute the rect of every pane in `node`, tiling `area` exactly.
pub(crate) fn layout(node: &LayoutNode, area: Rect, out: &mut Vec<(PaneId, Rect)>) {
    match node {
        LayoutNode::Leaf(id) => out.push((*id, area)),
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let (a, b) = divide(area, *direction, *ratio);
            layout(first, a, out);
            layout(second, b, out);
        }
    }
}

/// Split `area` into two rects along `direction`, giving `ratio` of the axis
/// to the first. Both halves stay at least one cell wide when the axis
/// allows it; a degenerate axis (length < 2) gives everything to the first.
fn divide(area: Rect, direction: SplitDirection, ratio: f32) -> (Rect, Rect) {
    match direction {
        SplitDirection::Horizontal => {
            let first_w = portion(area.width, ratio);
            (
                Rect::new(area.x, area.y, first_w, area.height),
                Rect::new(area.x + first_w, area.y, area.width - first_w, area.height),
            )
        }
        SplitDirection::Vertical => {
            let first_h = portion(area.height, ratio);
            (
                Rect::new(area.x, area.y, area.width, first_h),
                Rect::new(area.x, area.y + first_h, area.width, area.height - first_h),
            )
        }
    }
}

fn portion(total: u16, ratio: f32) -> u16 {
    if total < 2 {
        return total;
    }
    let ideal = (f32::from(total) * ratio).round() as u16;
    ideal.clamp(1, total - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(n: u64) -> PaneId {
        PaneId::from_raw(n)
    }

    fn rects_of(node: &LayoutNode, area: Rect) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        layout(node, area, &mut out);
        out
    }

    #[test]
    fn single_leaf_fills_area() {
        let node = LayoutNode::Leaf(pid(1));
        let area = Rect::new(0, 0, 80, 24);
        assert_eq!(rects_of(&node, area), vec![(pid(1), area)]);
    }

    #[test]
    fn even_horizontal_split_halves_width() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        let rects = rects_of(&node, Rect::new(0, 0, 80, 24));
        assert_eq!(rects[0].1, Rect::new(0, 0, 40, 24));
        assert_eq!(rects[1].1, Rect::new(40, 0, 40, 24));
    }

    #[test]
    fn odd_width_gives_remainder_deterministically() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        let rects = rects_of(&node, Rect::new(0, 0, 81, 24));
        assert_eq!(rects[0].1.width + rects[1].1.width, 81);
        assert_eq!(rects[0].1, Rect::new(0, 0, 41, 24));
        assert_eq!(rects[1].1, Rect::new(41, 0, 40, 24));
    }

    #[test]
    fn vertical_split_stacks() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        let rects = rects_of(&node, Rect::new(0, 0, 80, 25));
        assert_eq!(rects[0].1, Rect::new(0, 0, 80, 13));
        assert_eq!(rects[1].1, Rect::new(0, 13, 80, 12));
    }

    #[test]
    fn nested_splits_tile_exactly() {
        // ┌───┬───┐
        // │ 1 │ 2 │
        // │   ├───┤
        // │   │ 3 │
        // └───┴───┘
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Split {
                direction: SplitDirection::Vertical,
                ratio: 0.5,
                first: Box::new(LayoutNode::Leaf(pid(2))),
                second: Box::new(LayoutNode::Leaf(pid(3))),
            }),
        };
        let area = Rect::new(0, 0, 80, 24);
        let rects = rects_of(&node, area);
        assert_eq!(rects.len(), 3);
        let total: u32 = rects
            .iter()
            .map(|(_, r)| u32::from(r.width) * u32::from(r.height))
            .sum();
        assert_eq!(total, 80 * 24);
        assert_eq!(rects[0].1, Rect::new(0, 0, 40, 24));
        assert_eq!(rects[1].1, Rect::new(40, 0, 40, 12));
        assert_eq!(rects[2].1, Rect::new(40, 12, 40, 12));
    }

    #[test]
    fn extreme_ratio_keeps_both_panes_visible() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.99,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        let rects = rects_of(&node, Rect::new(0, 0, 10, 5));
        assert_eq!(rects[0].1.width, 9);
        assert_eq!(rects[1].1.width, 1);
    }

    #[test]
    fn degenerate_axis_collapses_second_pane() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        let rects = rects_of(&node, Rect::new(0, 0, 1, 5));
        assert_eq!(rects[0].1.width, 1);
        assert_eq!(rects[1].1.width, 0);
    }

    #[test]
    fn split_leaf_replaces_target() {
        let mut node = LayoutNode::Leaf(pid(1));
        assert!(node.split_leaf(pid(1), pid(2), SplitDirection::Horizontal));
        assert_eq!(node.leaves(), vec![pid(1), pid(2)]);
    }

    #[test]
    fn split_leaf_misses_unknown_target() {
        let mut node = LayoutNode::Leaf(pid(1));
        assert!(!node.split_leaf(pid(9), pid(2), SplitDirection::Horizontal));
        assert_eq!(node.leaves(), vec![pid(1)]);
    }

    #[test]
    fn remove_leaf_collapses_to_sibling() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        match node.remove_leaf(pid(1)) {
            RemoveOutcome::Removed(survivor) => {
                assert_eq!(survivor.leaves(), vec![pid(2)])
            }
            _ => panic!("expected Removed"),
        }
    }

    #[test]
    fn remove_only_leaf_reports_last() {
        let node = LayoutNode::Leaf(pid(1));
        assert!(matches!(node.remove_leaf(pid(1)), RemoveOutcome::LastLeaf));
    }

    #[test]
    fn remove_unknown_leaf_returns_tree_unchanged() {
        let node = LayoutNode::Split {
            direction: SplitDirection::Vertical,
            ratio: 0.5,
            first: Box::new(LayoutNode::Leaf(pid(1))),
            second: Box::new(LayoutNode::Leaf(pid(2))),
        };
        match node.remove_leaf(pid(9)) {
            RemoveOutcome::NotFound(tree) => {
                assert_eq!(tree.leaves(), vec![pid(1), pid(2)])
            }
            _ => panic!("expected NotFound"),
        }
    }
}
