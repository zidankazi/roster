//! The sidebar card context menu: a small popup of per-agent actions,
//! anchored at the right-click.
//!
//! Same visual language as the launcher and confirm modals — a raised
//! surface with the panel frame — but positioned at the pointer rather than
//! centered, and sized to its items. The binary owns the actions (pin,
//! close); this widget only draws the list and resolves which row a click
//! lands on. Geometry lives in free functions so render and hit-testing
//! share one source, the way `confirm.rs` does.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

use crate::launcher::{fill, frame};
use crate::style::{bright, danger, selected, SURFACE_RAISED};

/// One action the context menu offers for a sidebar card's agent. The
/// binary builds the item list (pin vs unpin depends on current state) and
/// acts on the chosen variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextMenuItem {
    /// Float this agent's card above the triage order.
    Pin,
    /// Return a pinned card to its triage position.
    Unpin,
    /// Close the pane, killing the agent (guarded by the confirm dialog
    /// when it is live — the binary routes through the same path as the
    /// title's `✕`).
    Close,
}

impl ContextMenuItem {
    /// The row label shown in the menu.
    pub fn label(self) -> &'static str {
        match self {
            ContextMenuItem::Pin => "pin to top",
            ContextMenuItem::Unpin => "unpin",
            ContextMenuItem::Close => "close",
        }
    }

    /// Whether the action destroys state — drawn in the danger red, like the
    /// confirm dialog's close button.
    pub fn destructive(self) -> bool {
        matches!(self, ContextMenuItem::Close)
    }
}

/// Horizontal padding inside the border, on each side of a label.
const H_PAD: u16 = 1;
/// The narrowest menu that still frames a label: border + one padded column
/// each side + at least one label cell. `menu_rect` floors to this and
/// `menu_drawable` refuses anything smaller, so the two can't drift.
const MIN_WIDTH: u16 = 2 + 2 * H_PAD + 1;

/// The widest label across `items`, in cells.
fn label_width(items: &[ContextMenuItem]) -> u16 {
    items
        .iter()
        .map(|item| item.label().chars().count() as u16)
        .max()
        .unwrap_or(0)
}

/// The menu's unclipped footprint for `items`: label column plus padding and
/// border, and one row per item between the top and bottom border rows.
fn menu_size(items: &[ContextMenuItem]) -> (u16, u16) {
    let width = (label_width(items) + 2 * H_PAD + 2).max(MIN_WIDTH);
    let height = items.len() as u16 + 2;
    (width, height)
}

/// The rect the menu occupies within `area`, anchored at `anchor` (the
/// clicked cell). It prefers its top-left corner at the anchor, but slides
/// back inside the frame when it would overflow the right or bottom edge —
/// a popup half off-screen would catch clicks past the edge. Clipped to
/// `area`; `menu_drawable` is how render and hit-testing agree it exists.
pub fn menu_rect(area: Rect, anchor: (u16, u16), items: &[ContextMenuItem]) -> Rect {
    let (width, height) = menu_size(items);
    let width = width.min(area.width);
    let height = height.min(area.height);
    let max_x = (area.x + area.width).saturating_sub(width);
    let max_y = (area.y + area.height).saturating_sub(height);
    let x = anchor.0.min(max_x).max(area.x);
    let y = anchor.1.min(max_y).max(area.y);
    Rect::new(x, y, width, height).intersection(area)
}

/// Whether a clipped menu rect is big enough to draw all its items. Render
/// bails when this is false and the hit tests consult it too — an invisible
/// menu must never own a click, or a phantom `close` row kills an agent with
/// no menu on screen.
fn menu_drawable(rect: Rect, items: &[ContextMenuItem]) -> bool {
    rect.width >= MIN_WIDTH && rect.height >= items.len() as u16 + 2
}

/// Whether a menu of `items` anchored at `anchor` would actually draw within
/// `area`. The binary gates opening the menu on this, so a frame too small
/// for it never enters a mode with nothing on screen.
pub fn menu_fits(area: Rect, anchor: (u16, u16), items: &[ContextMenuItem]) -> bool {
    menu_drawable(menu_rect(area, anchor, items), items)
}

/// Whether (`x`, `y`) falls inside the menu. Always false when the menu is
/// too small to draw, so any click dismisses it.
pub fn menu_contains(
    area: Rect,
    anchor: (u16, u16),
    items: &[ContextMenuItem],
    x: u16,
    y: u16,
) -> bool {
    let rect = menu_rect(area, anchor, items);
    menu_drawable(rect, items)
        && x >= rect.x
        && x < rect.x + rect.width
        && y >= rect.y
        && y < rect.y + rect.height
}

/// The item index under (`x`, `y`), when one is there. Border rows and
/// columns are dead space; only the item rows between them resolve.
pub fn menu_item_at(
    area: Rect,
    anchor: (u16, u16),
    items: &[ContextMenuItem],
    x: u16,
    y: u16,
) -> Option<usize> {
    let rect = menu_rect(area, anchor, items);
    if !menu_drawable(rect, items) || x < rect.x || x >= rect.x + rect.width {
        return None;
    }
    // Items sit on rows rect.y+1 .. rect.y+height-1; the border rows bracket
    // them.
    let top = rect.y + 1;
    if y < top || y >= rect.y + rect.height - 1 {
        return None;
    }
    let index = usize::from(y - top);
    (index < items.len()).then_some(index)
}

/// One frame's context-menu render inputs: the action items, the anchor
/// cell it was opened at, and the item currently under the pointer. Carried
/// on the [`crate::View`] so the binary hands render everything the menu
/// needs in one field.
pub struct ContextMenuView<'a> {
    /// The actions offered, top to bottom.
    pub items: &'a [ContextMenuItem],
    /// The cell the menu is anchored at (the right-click position).
    pub anchor: (u16, u16),
    /// The item under the pointer, for hover highlighting.
    pub hover: Option<usize>,
}

/// The context-menu widget: a bordered list of actions on the raised
/// surface, one row per item, the hovered row on the selected fill.
pub struct ContextMenu<'a> {
    items: &'a [ContextMenuItem],
    anchor: (u16, u16),
    hover: Option<usize>,
}

impl<'a> ContextMenu<'a> {
    /// A menu of `items` anchored at `anchor`, with no row hovered.
    pub fn new(items: &'a [ContextMenuItem], anchor: (u16, u16)) -> Self {
        ContextMenu {
            items,
            anchor,
            hover: None,
        }
    }

    /// The item index under the pointer, for hover highlighting.
    pub fn hover(mut self, index: Option<usize>) -> Self {
        self.hover = index;
        self
    }
}

impl Widget for ContextMenu<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rect = menu_rect(area, self.anchor, self.items);
        if !menu_drawable(rect, self.items) {
            return;
        }
        fill(buf, rect, SURFACE_RAISED);
        // A titleless frame — the menu's position at the card is its label.
        frame(buf, rect, "");
        let inner_x = rect.x + 1;
        let inner_w = usize::from(rect.width.saturating_sub(2));
        for (index, item) in self.items.iter().enumerate() {
            let y = rect.y + 1 + index as u16;
            if y >= rect.y + rect.height - 1 {
                break;
            }
            let hovered = self.hover == Some(index);
            // The hovered row takes the selected fill, so the pointer's row
            // reads as one continuous light bar (fg AND bg painted, never a
            // REVERSED overlay). Destructive actions keep the danger red on
            // either surface — a `close` row must always read as the one
            // that ends things.
            if hovered {
                buf.set_style(
                    Rect::new(inner_x, y, rect.width.saturating_sub(2), 1),
                    selected(),
                );
            }
            // Start from the row's surface tier, then let a destructive
            // action override the foreground to danger red on either surface.
            let mut style = if hovered {
                selected().add_modifier(Modifier::BOLD)
            } else {
                bright()
            };
            if item.destructive() {
                style = style.fg(danger());
            }
            let text = format!("{:pad$}{}", "", item.label(), pad = H_PAD as usize);
            buf.set_stringn(inner_x, y, text, inner_w, style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items() -> Vec<ContextMenuItem> {
        vec![ContextMenuItem::Pin, ContextMenuItem::Close]
    }

    #[test]
    fn menu_draws_items_on_the_raised_surface() {
        let area = Rect::new(0, 0, 80, 24);
        let anchor = (10, 4);
        let mut buf = Buffer::empty(area);
        ContextMenu::new(&items(), anchor).render(area, &mut buf);
        let rect = menu_rect(area, anchor, &items());
        let all: String = (rect.y..rect.y + rect.height)
            .map(|y| {
                (rect.x..rect.x + rect.width)
                    .map(|x| buf.cell((x, y)).unwrap().symbol().to_string())
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        assert!(menu_fits(area, anchor, &items()), "menu should fit here");
        assert!(all.contains("pin to top"), "missing pin row:\n{all}");
        assert!(all.contains("close"), "missing close row:\n{all}");
        // The menu is a raised surface, like every other piece of chrome.
        assert_eq!(
            buf.cell((rect.x, rect.y)).unwrap().style().bg,
            Some(SURFACE_RAISED)
        );
        // The close row is the danger red — the one action that ends things.
        let close_y = rect.y + 2;
        let close_col = (rect.x..rect.x + rect.width)
            .find(|x| buf.cell((*x, close_y)).unwrap().symbol() == "c")
            .expect("close row");
        assert_eq!(
            buf.cell((close_col, close_y)).unwrap().style().fg,
            Some(danger())
        );
    }

    #[test]
    fn items_hit_test_and_miss_the_border_and_outside() {
        let area = Rect::new(0, 0, 80, 24);
        let anchor = (10, 4);
        let items = items();
        let rect = menu_rect(area, anchor, &items);
        // Row for each item resolves to its index.
        assert_eq!(
            menu_item_at(area, anchor, &items, rect.x + 2, rect.y + 1),
            Some(0)
        );
        assert_eq!(
            menu_item_at(area, anchor, &items, rect.x + 2, rect.y + 2),
            Some(1)
        );
        // The top and bottom border rows are dead space.
        assert_eq!(menu_item_at(area, anchor, &items, rect.x + 2, rect.y), None);
        assert_eq!(
            menu_item_at(area, anchor, &items, rect.x + 2, rect.y + rect.height - 1),
            None
        );
        // A column outside the rect misses; a point inside it contains.
        assert_eq!(
            menu_item_at(area, anchor, &items, rect.x + rect.width, rect.y + 1),
            None
        );
        assert!(menu_contains(area, anchor, &items, rect.x + 1, rect.y + 1));
        assert!(!menu_contains(
            area,
            anchor,
            &items,
            rect.x.saturating_sub(1),
            rect.y
        ));
    }

    #[test]
    fn menu_stays_inside_the_frame_when_anchored_at_an_edge() {
        // A right-click in the bottom-right corner must not push the menu
        // off-screen — it slides back so every row stays clickable inside
        // the frame.
        let area = Rect::new(0, 0, 80, 24);
        let items = items();
        let anchor = (79, 23);
        let rect = menu_rect(area, anchor, &items);
        assert!(
            rect.x + rect.width <= area.x + area.width,
            "overflows right"
        );
        assert!(
            rect.y + rect.height <= area.y + area.height,
            "overflows bottom"
        );
        assert!(
            menu_drawable(rect, &items),
            "should still draw at the corner"
        );
        // The last item row is inside the frame and hits.
        assert_eq!(
            menu_item_at(
                area,
                anchor,
                &items,
                rect.x + 2,
                rect.y + items.len() as u16
            ),
            Some(items.len() - 1)
        );
    }

    #[test]
    fn sliver_frames_neither_draw_nor_hit() {
        // Frames under the menu's minimum footprint draw nothing (buffer
        // equality catches style-only writes too) and own no click — a
        // phantom `close` row here would kill an agent with no menu on
        // screen.
        let items = items();
        for (w, h) in [(1u16, 1u16), (MIN_WIDTH - 1, 24), (80, 3)] {
            let area = Rect::new(0, 0, w, h);
            let anchor = (0, 0);
            let mut buf = Buffer::empty(area);
            ContextMenu::new(&items, anchor).render(area, &mut buf);
            assert!(!menu_fits(area, anchor, &items), "claims to fit at {w}x{h}");
            assert_eq!(buf, Buffer::empty(area), "drawn at {w}x{h}");
            for y in 0..h {
                for x in 0..w {
                    assert_eq!(
                        menu_item_at(area, anchor, &items, x, y),
                        None,
                        "phantom item {w}x{h}"
                    );
                    assert!(
                        !menu_contains(area, anchor, &items, x, y),
                        "phantom contains {w}x{h}"
                    );
                }
            }
        }
    }
}
