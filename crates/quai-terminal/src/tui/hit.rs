//! What is under the pointer.
//!
//! Drawing records every clickable thing as it draws it: a region of the frame and what it
//! means ([`Target`]). The last region recorded is on top, as on screen. A modal *captures* the
//! pointer when it draws (see [`HitMap::capture`]): everything recorded before it becomes inert
//! and the rest of the screen answers as a backdrop, so no click lands on the dashboard through a
//! review.
//!
//! Targets name what a click means, not what key it resembles; `pointer` turns them into the
//! same state changes the keyboard makes. None of them can sign: approving a review from the
//! pointer only arms the button, and the key that signs is the keyboard's.

use super::app::{Screen, Section};
use ratatui::layout::Rect;

/// A list that scrolls on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ListId {
    /// A screen's list in a pane.
    Screen(Screen, usize),
    /// The list inside the open detail view.
    Detail,
    /// A collection detail's active listings (beside its item grid).
    DetailListings,
    Palette,
    TokenPicker,
    Themes,
    Gallery,
    Glossary,
    /// The wallet switcher.
    Wallets,
    /// The account picker.
    Accounts,
    /// The action sheet.
    Sheet,
}

/// The review modal's parts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewPart {
    Reject,
    /// Arms the approve button, the same as Tab; it never signs.
    Approve,
}

/// Parts of the header bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderPart {
    Wallet,
    /// The account that acts, beside the wallet's name.
    Account,
    Network,
    Unread,
}

/// Buttons in modals that aren't list rows or form fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Close a modal, as Esc does.
    Close,
    /// Receive: show QUAI or Qi.
    ReceiveAsset(bool),
    /// Receive: copy the shown address or code.
    CopyReceive,
}

/// What a region means.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Section(Section),
    /// A sub-tab of the current section (index into its tab labels).
    Tab(usize),
    /// A footer hint: the key it names, pressed.
    Key(crossterm::event::KeyCode),
    /// A pane of the current screen: a click focuses it.
    Pane(usize),
    /// A row of a list. `key` is what the row showed when it was drawn, so a list that re-sorted
    /// between the frame and the click still picks the row that was under the pointer.
    Row {
        list: ListId,
        index: usize,
        key: Option<String>,
    },
    /// Something that scrolls with the wheel and has no rows of its own (a modal body).
    Scroll(Scroll),
    Header(HeaderPart),
    Toast,
    Review(ReviewPart),
    /// A confirmation's yes (true) or no (false).
    Confirm(bool),
    /// A form field (focus it).
    Field(usize),
    /// A field of the screen's card (Swap, Convert, Wrap, a deposit): focus it.
    CardField(usize),
    /// An option of a form's choice field.
    Choice {
        field: usize,
        option: usize,
    },
    Button(Button),
    /// A go-to destination.
    Route(super::app::Screen),
    /// Inside a modal, nothing clickable: the click is used up.
    Swallow,
    /// Outside the open modal.
    Backdrop,
}

/// Something the wheel scrolls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scroll {
    Review,
    Help,
    Form,
    /// The Markets chart: the wheel steps its timeframe.
    Chart,
}

/// This frame's clickable regions.
#[derive(Default, Debug)]
pub struct HitMap {
    regions: Vec<(Rect, Target)>,
    /// Regions below this index are behind a modal.
    floor: usize,
    /// Each list drawn this frame: its rows' area, the first item shown, and how many there are
    /// (for scrollbars).
    lists: Vec<(Rect, usize, usize)>,
}

impl HitMap {
    pub fn clear(&mut self) {
        self.regions.clear();
        self.lists.clear();
        self.floor = 0;
    }

    pub fn add(&mut self, rect: Rect, target: Target) {
        if rect.width > 0 && rect.height > 0 {
            self.regions.push((rect, target));
        }
    }

    /// One region per visible row of a list: row `i` of the window is item `offset + i`.
    pub fn rows(&mut self, list: ListId, area: Rect, offset: usize, len: usize, key: impl Fn(usize) -> Option<String>) {
        self.lists.push((area, offset, len));
        for i in 0..area.height as usize {
            let index = offset + i;
            if index >= len {
                break;
            }
            let rect = Rect::new(area.x, area.y + i as u16, area.width, 1);
            self.add(rect, Target::Row { list, index, key: key(index) });
        }
    }

    /// Record targets on a one-line strip of spans (a tab bar, the header, the footer): span `i`
    /// gets `target(i)`, over exactly the cells it occupies, clipped to `area`.
    pub fn spans(&mut self, area: Rect, spans: &[ratatui::text::Span], target: impl Fn(usize) -> Option<Target>) {
        let mut x = area.x;
        for (i, span) in spans.iter().enumerate() {
            let w = span.width() as u16;
            if x >= area.right() {
                break;
            }
            if let Some(t) = target(i) {
                self.add(Rect::new(x, area.y, w.min(area.right() - x), 1), t);
            }
            x = x.saturating_add(w);
        }
    }

    /// A modal opened over `whole`: everything recorded so far goes inert, and whatever of
    /// `whole` the modal doesn't claim answers as the backdrop.
    pub fn capture(&mut self, whole: Rect) {
        self.floor = self.regions.len();
        self.regions.push((whole, Target::Backdrop));
    }

    /// Whether a modal has the pointer this frame.
    #[cfg(test)]
    pub fn captured(&self) -> bool {
        self.floor > 0 || matches!(self.regions.first(), Some((_, Target::Backdrop)))
    }

    /// The topmost live region at a cell.
    pub fn at(&self, x: u16, y: u16) -> Option<&Target> {
        self.regions[self.floor..].iter().rev().find(|(r, _)| contains(*r, x, y)).map(|(_, t)| t)
    }

    /// The topmost live region at a cell, with its rectangle.
    pub fn region_at(&self, x: u16, y: u16) -> Option<(Rect, &Target)> {
        self.regions[self.floor..].iter().rev().find(|(r, _)| contains(*r, x, y)).map(|(r, t)| (*r, t))
    }

    /// Every live region (above a modal's capture).
    pub fn live_regions(&self) -> &[(Rect, Target)] {
        &self.regions[self.floor..]
    }

    /// The lists drawn this frame (see `lists`).
    pub fn lists(&self) -> &[(Rect, usize, usize)] {
        &self.lists
    }

    /// What the wheel would scroll at a cell: the topmost list row or scroll area there.
    pub fn scrollable_at(&self, x: u16, y: u16) -> Option<&Target> {
        self.regions[self.floor..]
            .iter()
            .rev()
            .filter(|(r, _)| contains(*r, x, y))
            .map(|(_, t)| t)
            .find(|t| matches!(t, Target::Row { .. } | Target::Scroll(_) | Target::Backdrop))
            .filter(|t| !matches!(t, Target::Backdrop))
    }

    /// Every live region (for tests and the hit-map goldens).
    #[cfg(test)]
    pub fn live(&self) -> &[(Rect, Target)] {
        &self.regions[self.floor..]
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

/// A list's own scroll position. The window follows the selection only when the selection
/// would leave it (so a click never scrolls the row out from under the pointer), and the wheel
/// can move the window without moving the selection (`pinned`) until the keyboard moves it again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListState {
    pub offset: usize,
    pub pinned: bool,
}

impl ListState {
    /// The first visible item, for `len` items in `rows` rows with `selected` selected.
    pub fn window(&mut self, selected: usize, len: usize, rows: usize) -> usize {
        if rows == 0 {
            return 0;
        }
        if !self.pinned {
            if selected < self.offset {
                self.offset = selected;
            } else if selected >= self.offset + rows {
                self.offset = selected + 1 - rows;
            }
        }
        self.offset = self.offset.min(len.saturating_sub(rows));
        self.offset
    }

    /// The wheel: move the window, leave the selection.
    pub fn scroll(&mut self, delta: i64) {
        self.offset = (self.offset as i64 + delta).max(0) as usize;
        self.pinned = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_top_region_wins_and_a_modal_captures() {
        let mut h = HitMap::default();
        h.add(Rect::new(0, 0, 10, 10), Target::Pane(0));
        h.add(Rect::new(2, 2, 3, 1), Target::Tab(1));
        assert_eq!(h.at(3, 2), Some(&Target::Tab(1)));
        assert_eq!(h.at(1, 1), Some(&Target::Pane(0)));
        h.capture(Rect::new(0, 0, 10, 10));
        h.add(Rect::new(4, 4, 2, 2), Target::Confirm(false));
        assert_eq!(h.at(3, 2), Some(&Target::Backdrop), "behind the modal, nothing answers");
        assert_eq!(h.at(4, 4), Some(&Target::Confirm(false)));
        assert!(h.captured());
        assert_eq!(h.at(20, 20), None);
    }

    #[test]
    fn a_window_follows_the_selection_only_when_it_must() {
        let mut s = ListState::default();
        assert_eq!(s.window(0, 100, 10), 0);
        assert_eq!(s.window(9, 100, 10), 0, "still in view: no scroll");
        assert_eq!(s.window(10, 100, 10), 1, "one past the end: one row");
        assert_eq!(s.window(5, 100, 10), 1, "a click on a visible row leaves the window where it is");
        assert_eq!(s.window(0, 100, 10), 0);
        s.scroll(30);
        assert_eq!(s.window(0, 100, 10), 30, "the wheel moves the window, not the selection");
        s.pinned = false;
        assert_eq!(s.window(0, 100, 10), 0, "the keyboard brings the selection back into view");
        s.scroll(500);
        assert_eq!(s.window(0, 100, 10), 90, "clamped to the last page");
    }

    #[test]
    fn rows_record_their_items() {
        let mut h = HitMap::default();
        h.rows(ListId::Palette, Rect::new(0, 5, 20, 4), 10, 12, |i| Some(format!("item{i}")));
        assert_eq!(h.live().len(), 2, "only the rows that exist");
        assert_eq!(h.at(0, 6), Some(&Target::Row { list: ListId::Palette, index: 11, key: Some("item11".into()) }));
        assert!(h.scrollable_at(0, 5).is_some());
    }
}
