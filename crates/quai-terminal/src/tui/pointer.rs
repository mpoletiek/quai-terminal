//! The mouse: what a press, a release, a double-click or the wheel does.
//!
//! Every target resolves to a state change the keyboard can also make, through the same code:
//! focus a pane, select a row, switch a tab, arm a button. Rules that keep a pointer from moving
//! money on its own:
//!
//! - A click acts on release, and only when press and release land on the same target; dragging
//!   off a button cancels it.
//! - Presses in the first [`MODAL_GRACE`] after a modal appears are ignored, and so is the first
//!   press after the window regains focus ([`FOCUS_GRACE`]): the click that focuses a window or
//!   was meant for what the modal replaced must not land on the modal.
//! - The review's Approve only arms (the same as Tab); signing is Enter on the keyboard. A
//!   confirmation that accepts a stranger's payment channel or moves to mainnet arms the same way.
//! - A row is resolved by the key it showed when drawn, so a list that re-sorted since the frame
//!   picks the row that was under the pointer, or nothing.
//! - Clicks, the wheel and drags count as presence for auto-lock; a pointer merely resting on the
//!   window does not.

use super::app::{App, Card, ConfirmAction, FieldKind, Modal, Screen};
use super::hit::{Button, HeaderPart, ListId, ReviewPart, Scroll, Target};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::time::{Duration, Instant};

/// Presses this soon after a modal appears are ignored.
pub const MODAL_GRACE: Duration = Duration::from_millis(300);
/// The first press this soon after the window regains focus is ignored.
pub const FOCUS_GRACE: Duration = Duration::from_millis(150);
/// Two clicks on the same target within this are a double-click.
pub const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows the wheel moves per notch.
pub const WHEEL_ROWS: i64 = 3;

/// The pointer's state between events.
#[derive(Debug, Default)]
pub struct PointerState {
    /// Where the left button went down, and what was there.
    pressed: Option<Target>,
    /// The last completed click, for double-click.
    last_click: Option<(Target, Instant)>,
    /// What the pointer is over (hover).
    pub hover: Option<Target>,
    /// Where the pointer is (cell), while the terminal reports motion.
    pub at: Option<(u16, u16)>,
    /// Where the last drag step was (for panning the chart).
    pub drag_x: Option<u16>,
    /// The explorer link the left button went down on, if any.
    pressed_link: Option<String>,
}

impl ConfirmAction {
    /// Confirmations a click only arms: accepting a sender you may not know, and moving to the
    /// network with real money. The key finishes them.
    pub fn sensitive(&self) -> bool {
        match self {
            ConfirmAction::AcceptOffer(_) => true,
            ConfirmAction::SwitchNetwork(id) => id == "mainnet",
            _ => false,
        }
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Whether a key pressed on the pointer's behalf could finish something that moves money: Enter
/// while a review is open (it signs when Approve is armed), and `y` on a confirmation that only
/// the keyboard may finish.
fn deliberate_only(modal: &Modal, code: KeyCode) -> bool {
    match modal {
        Modal::Review(_) => !matches!(code, KeyCode::Esc),
        Modal::Confirm { action, .. } => action.sensitive() && matches!(code, KeyCode::Char('y') | KeyCode::Char('Y')),
        _ => false,
    }
}

impl App {
    /// The pointer's shape over what it is on (OSC 22): a hand on what a click does something
    /// to, a text cursor on fields, and not-allowed on an Approve not yet read to the end.
    pub fn pointer_shape(&self) -> &'static str {
        use super::hit::ReviewPart;
        match &self.input.pointer.hover {
            None | Some(Target::Swallow | Target::Backdrop | Target::Scroll(_)) => "default",
            Some(Target::Field(_) | Target::CardField(_)) => "text",
            Some(Target::Review(ReviewPart::Approve)) => match &self.modal {
                Modal::Review(r) if !r.can_approve() => "not-allowed",
                _ => "pointer",
            },
            Some(_) => "pointer",
        }
    }

    /// Handle a mouse event. `size` is the terminal's, as for keys.
    pub fn on_mouse(&mut self, m: MouseEvent, size: (u16, u16)) {
        let (x, y) = (m.column, m.row);
        match m.kind {
            MouseEventKind::Moved => {
                let over = self.input.hits.borrow().at(x, y).cloned();
                // Onto or off a shown hash or address: its tooltip comes or goes.
                let link = |at: Option<(u16, u16)>| {
                    at.and_then(|(x, y)| self.term.links_shown.borrow().iter().position(|l| l.y == y && (l.x..l.end).contains(&x)))
                };
                // Over the chart the crosshair follows the pointer cell by cell.
                let crossed = link(self.input.pointer.at) != link(Some((x, y)))
                    || (matches!(over, Some(Target::Scroll(Scroll::Chart))) && self.input.pointer.at != Some((x, y)));
                self.input.pointer.at = Some((x, y));
                if over != self.input.pointer.hover || crossed {
                    self.input.pointer.hover = over;
                    self.dirty = true;
                }
            }
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                self.note_input();
                let delta = if m.kind == MouseEventKind::ScrollDown { WHEEL_ROWS } else { -WHEEL_ROWS };
                self.wheel(x, y, delta);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.note_input();
                self.input.pointer.drag_x = Some(x);
                self.input.pointer.pressed = if self.in_grace() { None } else { self.input.hits.borrow().at(x, y).cloned() };
                self.input.pointer.pressed_link = if self.in_grace() { None } else { self.link_at(x, y) };
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Only something a click acts on counts as being under the link: a scrolling body
                // or a backdrop does not.
                let on_target = matches!(
                    self.input.hits.borrow().at(x, y),
                    Some(t) if !matches!(t, Target::Scroll(_) | Target::Swallow | Target::Backdrop)
                );
                if let Some(url) = self.input.pointer.pressed_link.take()
                    && self.link_at(x, y).as_deref() == Some(url.as_str())
                    && self.click_link(&url, m.modifiers, on_target)
                {
                    self.input.pointer.pressed = None;
                    self.dirty = true;
                    return;
                }
                let Some(pressed) = self.input.pointer.pressed.take() else { return };
                let released = self.input.hits.borrow().at(x, y).cloned();
                if released.as_ref() != Some(&pressed) {
                    return;
                }
                let now = Instant::now();
                let double =
                    self.input.pointer.last_click.as_ref().is_some_and(|(t, at)| *t == pressed && now.duration_since(*at) <= DOUBLE_CLICK);
                self.input.pointer.last_click = if double { None } else { Some((pressed.clone(), now)) };
                self.activate(pressed, double, size);
                self.dirty = true;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.note_input();
                // Dragging the chart pans it: right looks further back, left comes forward.
                if matches!(self.input.pointer.pressed, Some(Target::Scroll(Scroll::Chart))) {
                    if let Some(from) = self.input.pointer.drag_x {
                        let pool = self.selected_pool().map(|p| p.address.clone()).unwrap_or_default();
                        let timeframe = self.eco.markets_view.timeframe;
                        let mv = &mut self.eco.markets_view;
                        if mv.pan.1 != pool || mv.pan.2 != timeframe {
                            mv.pan = (0, pool, timeframe);
                        }
                        let moved = i64::from(x) - i64::from(from);
                        mv.pan.0 = (mv.pan.0 as i64 + moved).clamp(0, super::eco::MARKET_CANDLES as i64 * 3) as usize;
                        self.dirty = true;
                    }
                    self.input.pointer.drag_x = Some(x);
                }
            }
            // Right-click: the row under the pointer becomes the focus, and its actions open —
            // the same sheet space opens, so a menu never offers what the keys don't.
            MouseEventKind::Down(MouseButton::Right) => {
                self.note_input();
                if self.in_grace() || !matches!(self.modal, Modal::None) {
                    return;
                }
                let under = self.input.hits.borrow().at(x, y).cloned();
                if let Some(Target::Row { list, index, key: shown }) = under
                    && let Some(index) = self.resolve_row(list, index, shown.as_deref())
                {
                    self.select_row(list, index);
                }
                self.open_sheet();
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// The explorer link drawn at `(x, y)`, if any.
    fn link_at(&self, x: u16, y: u16) -> Option<String> {
        self.term.links_shown.borrow().iter().find(|l| l.y == y && (l.x..l.end).contains(&x)).map(|l| l.url.clone())
    }

    /// A click on an explorer link. Ctrl opens it in the browser, and so does a plain click where
    /// nothing else is under the pointer. Alt copies the id it shows. A plain click on a row that
    /// shows an id stays a click on the row, and never copies: a clipboard is where someone keeps
    /// the address they are about to paste, and a click must not replace it unasked. Returns
    /// whether the click was the link's.
    fn click_link(&mut self, url: &str, modifiers: KeyModifiers, on_target: bool) -> bool {
        if modifiers.contains(KeyModifiers::ALT) {
            let Some(id) = url.rsplit('/').next().filter(|id| id.starts_with("0x")) else { return false };
            let what = if id.len() == 66 { "hash" } else { "address" };
            self.copy(super::clipboard::PublicText::shown(id), what);
            return true;
        }
        if !modifiers.contains(KeyModifiers::CONTROL) && on_target {
            return false;
        }
        match super::browser::open(url) {
            Ok(_) => {
                self.info(format!("opening the explorer · {}", wallet_core::session::short_address(url.rsplit('/').next().unwrap_or(url))))
            }
            Err(why) => {
                self.toast(format!("couldn't open the browser ({why}); copied the link instead"), true);
                self.copy(super::clipboard::PublicText::link(url), "link");
            }
        }
        true
    }

    /// The one way the pointer presses a key. It refuses the keys that finish a review or a
    /// sensitive confirmation, so no click, however it is routed, can reach them: the only key
    /// it may press on an open review is Esc, which rejects.
    pub(crate) fn press(&mut self, code: KeyCode, size: (u16, u16)) {
        if deliberate_only(&self.modal, code) {
            return;
        }
        self.on_key(key(code), size);
    }

    /// Whether a press now would be too soon after a modal appeared or the window was focused.
    fn in_grace(&self) -> bool {
        let modal = !matches!(self.modal, Modal::None) && self.input.modal_since.get().is_some_and(|at| at.elapsed() < MODAL_GRACE);
        let focus = self.term.focus_gained_at.is_some_and(|at| at.elapsed() < FOCUS_GRACE);
        modal || focus
    }

    /// The wheel over `(x, y)`: the list or body under the pointer scrolls, whatever has focus.
    fn wheel(&mut self, x: u16, y: u16, delta: i64) {
        let target = self.input.hits.borrow().scrollable_at(x, y).cloned();
        match target {
            Some(Target::Row { list, .. }) => {
                self.input.lists.borrow_mut().entry(list).or_default().scroll(delta);
                self.dirty = true;
            }
            Some(Target::Scroll(Scroll::Review)) => {
                // The same as the arrow keys: scrolling is reading, and the read-to-the-end rule
                // counts it the same way.
                self.move_selection(delta);
            }
            Some(Target::Scroll(Scroll::Help)) => {
                self.nav.help_scroll = (self.nav.help_scroll as i64 + delta).max(0) as u16;
                self.dirty = true;
            }
            Some(Target::Scroll(Scroll::Chart)) => {
                // Down for longer candles, up for shorter, as a map zooms out and in.
                let n = wallet_core::markets::TIMEFRAMES.len();
                let mv = &mut self.eco.markets_view;
                mv.timeframe = if delta > 0 { (mv.timeframe + 1).min(n - 1) } else { mv.timeframe.saturating_sub(1) };
                let label = wallet_core::markets::TIMEFRAMES[mv.timeframe].0;
                self.info(format!("chart: {label} candles"));
            }
            Some(Target::Scroll(Scroll::Form)) => {
                let code = if delta > 0 { KeyCode::Down } else { KeyCode::Up };
                let (w, h) = self.term.last_size;
                self.press(code, (w, h));
            }
            _ => {}
        }
    }

    /// A completed click (or double-click) on `target`.
    fn activate(&mut self, target: Target, double: bool, size: (u16, u16)) {
        match target {
            Target::Section(s) => self.switch_section(s),
            Target::Tab(i) => self.select_tab(i),
            Target::Key(code) => {
                // Footer hints are screen keys; under a modal they are behind the capture and
                // never reach here, and none of them is a key that signs.
                if matches!(self.modal, Modal::None) {
                    self.press(code, size);
                }
            }
            Target::Pane(p) => {
                if matches!(self.modal, Modal::None) && p < self.nav.screen.panes() {
                    self.nav.pane = p;
                    self.dock.focus = false;
                }
            }
            Target::Row { list, index, key: shown } => {
                let Some(index) = self.resolve_row(list, index, shown.as_deref()) else { return };
                self.select_row(list, index);
                if double {
                    self.open_row(list, size);
                }
            }
            Target::Header(HeaderPart::Wallet) => self.open_wallet_switcher(),
            Target::Header(HeaderPart::Account) => self.open_account_picker(),
            Target::Header(HeaderPart::Network) => self.switch(super::app::Screen::Network),
            Target::Header(HeaderPart::Unread) => self.press(KeyCode::Char('N'), size),
            Target::Toast => self.status.toasts.clear(),
            Target::Review(ReviewPart::Reject) => self.press(KeyCode::Esc, size),
            Target::Review(ReviewPart::Approve) => {
                if let Modal::Review(r) = &mut self.modal {
                    r.approve_focused = true;
                }
                let can = matches!(&self.modal, Modal::Review(r) if r.can_approve());
                self.toast(if can { "approve is armed · press enter to sign" } else { "read to the end of the review first" }, !can);
            }
            Target::Route(place) => {
                self.modal = Modal::None;
                self.go(place);
            }
            Target::Scroll(_) | Target::Swallow => {}
            Target::Confirm(false) => self.press(KeyCode::Char('n'), size),
            Target::Confirm(true) => {
                let sensitive = matches!(&self.modal, Modal::Confirm { action, .. } if action.sensitive());
                if sensitive {
                    self.info("press y to confirm");
                } else {
                    self.press(KeyCode::Char('y'), size);
                }
            }
            Target::Field(i) => {
                if let Modal::Form(form) = &mut self.modal
                    && i < form.fields.len()
                    && !form.pending
                {
                    form.focus = i;
                }
            }
            // A card's field: focus it, as Tab would have. (Typing, a token pick and review stay
            // with the keys.)
            Target::CardField(n) => match self.nav.screen {
                Screen::Exchange => match self.nav.card {
                    Card::Swap => self.eco.swap.field = n,
                    Card::Convert => self.eco.convert.field = n,
                    Card::Wrap => self.eco.wrap.field = n,
                },
                Screen::Pools => {
                    if let Some(add) = self.eco.pools_view.add.as_mut() {
                        add.field = n;
                    }
                }
                _ => {}
            },
            Target::Choice { field, option } => {
                if let Modal::Form(form) = &mut self.modal
                    && !form.pending
                    && let Some(f) = form.fields.get_mut(field)
                    && let FieldKind::Choice(options) = &f.kind
                    && let Some((value, _)) = options.get(option)
                {
                    f.value = value.clone();
                    form.focus = field;
                }
            }
            Target::Button(Button::Close) => self.press(KeyCode::Esc, size),
            Target::Button(Button::ReceiveAsset(qi)) => {
                if let Modal::Receive { asset_qi, .. } = &mut self.modal {
                    *asset_qi = qi;
                }
            }
            Target::Button(Button::CopyReceive) => self.press(KeyCode::Char('y'), size),
            Target::Backdrop => {
                // Clicking beside something you are reading closes it; beside something you are
                // filling in or approving, nothing happens (a stray click must not lose a typed
                // form or reject a review).
                let dismissible = matches!(
                    self.modal,
                    Modal::Help
                        | Modal::Glossary { .. }
                        | Modal::Palette { .. }
                        | Modal::Notifications
                        | Modal::TokenPicker { .. }
                        | Modal::Receive { .. }
                        | Modal::Result(_)
                        | Modal::Quote(_)
                        | Modal::Notice { .. }
                        | Modal::Effects(_)
                        | Modal::Sheet { .. }
                        | Modal::GoTo
                );
                if dismissible {
                    self.press(KeyCode::Esc, size);
                }
            }
        }
    }

    /// The index a clicked row stands for now: the drawn index if it still shows the same item,
    /// else wherever that item moved to, else nothing (it is gone).
    fn resolve_row(&self, list: ListId, index: usize, shown: Option<&str>) -> Option<usize> {
        let Some(shown) = shown else { return Some(index) };
        match self.row_key(list, index) {
            // A list without keys to compare: the drawn index is all there is.
            None if (0..self.row_count(list)).all(|i| self.row_key(list, i).is_none()) => Some(index),
            Some(now) if now == shown => Some(index),
            _ => (0..self.row_count(list)).find(|&i| self.row_key(list, i).as_deref() == Some(shown)),
        }
    }

    fn select_row(&mut self, list: ListId, index: usize) {
        match list {
            ListId::Screen(screen, pane) => {
                if self.nav.screen == screen && self.nav.detail.is_empty() {
                    self.nav.pane = pane;
                    self.dock.focus = false;
                    // Pools keeps a cursor per pane of its own.
                    match (screen, pane) {
                        (super::app::Screen::Pools, 0) => self.eco.pools_view.selected = index,
                        (super::app::Screen::Pools, _) => self.eco.pools_view.pool_selected = index,
                        _ => self.nav.selected = index,
                    }
                }
            }
            ListId::Detail => self.nav.detail_selected = index,
            ListId::DetailListings => self.eco.nft.listing = index,
            ListId::Palette => {
                if let Modal::Palette { selected, .. } = &mut self.modal {
                    *selected = index;
                }
            }
            ListId::TokenPicker => {
                if let Modal::TokenPicker { selected, .. } = &mut self.modal {
                    *selected = index;
                }
            }
            ListId::Themes => {
                if let Modal::Themes(picker) = &mut self.modal
                    && let Some(&entry) = picker.visible().get(index)
                {
                    picker.selected = entry;
                    if let Some(e) = picker.current() {
                        self.theme = e.theme.clone();
                    }
                }
            }
            ListId::Gallery => {
                if let Modal::Effects(g) = &mut self.modal {
                    g.selected = index;
                    g.preview = None;
                }
            }
            ListId::Glossary => {
                if let Modal::Glossary { selected } = &mut self.modal {
                    *selected = index;
                }
            }
            ListId::Wallets => {
                if let Modal::Wallets { selected } = &mut self.modal {
                    *selected = index;
                }
            }
            ListId::Accounts => {
                if let Modal::Accounts { selected } = &mut self.modal {
                    *selected = index;
                }
            }
            ListId::Sheet => {
                // One click is enough: the sheet's items are actions, like buttons.
                if let Modal::Sheet { items, .. } = &self.modal
                    && let Some(item) = items.get(index).cloned()
                {
                    self.modal = Modal::None;
                    self.run_do(item.how);
                }
            }
        }
    }

    /// A double-click: what Enter does on that row.
    fn open_row(&mut self, list: ListId, size: (u16, u16)) {
        let enter_opens = match list {
            // Screen lists open a detail (or run the row's action); the modal lists choose.
            ListId::Screen(..) | ListId::Detail | ListId::DetailListings => matches!(self.modal, Modal::None),
            ListId::Palette | ListId::TokenPicker | ListId::Themes | ListId::Gallery | ListId::Wallets | ListId::Accounts => true,
            ListId::Glossary | ListId::Sheet => false,
        };
        if enter_opens {
            self.press(KeyCode::Enter, size);
        }
    }

    /// What row `index` of a list shows, as a stable key: an address, an id, a hash. `None` for
    /// lists whose rows never move under the pointer (a click then uses the drawn index).
    pub(crate) fn row_key(&self, list: ListId, index: usize) -> Option<String> {
        use super::app::Screen;
        let activity_key = |app: &App, i: usize| app.activity_rows().get(i).map(|r| app.activity_row_key(r));
        match list {
            ListId::Screen(Screen::Home, 0) => self.eco.feeds.portfolio.value().and_then(|p| p.rows.get(index)).map(|r| r.key.id()),
            ListId::Screen(Screen::Home, _) | ListId::Screen(Screen::Activity, _) => activity_key(self, index),
            ListId::Screen(Screen::Accounts, _) => self.dash.accounts.get(index).map(|a| a.address.clone()),
            ListId::Screen(Screen::Qi, _) => self.dash.qi.as_ref().and_then(|q| q.coins.get(index)).map(|c| c.outpoint.clone()),
            ListId::Screen(Screen::Contacts, 0) => self.dash.contacts.get(index).map(|c| c.name.clone()),
            ListId::Screen(Screen::Contacts, _) => {
                let offers = self.dash.offers.len();
                if index < offers {
                    self.dash.offers.get(index).map(|o| o.code.clone())
                } else {
                    self.dash.peers.get(index - offers).map(|p| p.code.clone())
                }
            }
            ListId::Screen(Screen::Wallets, _) => self.cockpit.list.get(index).map(|w| w.id.clone()),
            ListId::Screen(Screen::Markets, 0) => self.market_rows().get(index).map(|p| p.address.clone()),
            ListId::Screen(Screen::Markets, _) => self.flow_rows().get(index).map(|s| s.tx.clone()),
            ListId::Screen(Screen::Pools, 0) => self.position_rows().get(index).map(|p| p.pair.clone()),
            ListId::Screen(Screen::Launches, _) => self.launch_rows().get(index).map(|l| l.token.clone()),
            ListId::Screen(Screen::Explore, _) => self.eco.collections_filtered().get(index).map(|c| c.address.clone()),
            ListId::Screen(Screen::Listings, _) => self.eco.visible_listings().get(index).map(|l| format!("{}:{}", l.contract, l.token_id)),
            ListId::Screen(Screen::Orders, _) => self.eco.feeds.orders.value().and_then(|o| o.get(index)).map(|p| p.id.clone()),
            ListId::Palette => match &self.modal {
                Modal::Palette { query, .. } => self.palette_entries(query).get(index).map(|e| e.label.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    /// How many rows a list has now (for finding a row that moved).
    pub(crate) fn row_count(&self, list: ListId) -> usize {
        match list {
            ListId::Screen(screen, pane) if screen == self.nav.screen && pane == self.nav.pane => self.list_len(),
            ListId::Screen(super::app::Screen::Home, 0) => self.eco.feeds.portfolio.value().map_or(0, |p| p.rows.len()),
            ListId::Screen(super::app::Screen::Home, _) => self.activity_rows().len(),
            ListId::Detail => self.detail_len(),
            ListId::Palette => match &self.modal {
                Modal::Palette { query, .. } => self.palette_entries(query).len(),
                _ => 0,
            },
            _ => 0,
        }
    }

    /// The stable key of an activity row (an operation id or an activity key).
    pub(crate) fn activity_row_key(&self, &(_, is_op, j): &(u64, bool, usize)) -> String {
        if is_op { self.dash.ops[j].id.clone() } else { self.dash.activity[j].key.clone() }
    }

    /// The first visible item of `list`, drawn in `rows` rows (see `ListState::window`).
    pub(crate) fn list_window(&self, list: ListId, selected: usize, len: usize, rows: usize) -> usize {
        self.input.lists.borrow_mut().entry(list).or_default().window(selected, len, rows)
    }

    /// The window of a pane's list: it follows the selection while that pane has focus, and
    /// otherwise stays where it was (the screen shares one selection between its panes).
    pub(crate) fn pane_window(&self, list: ListId, len: usize, rows: usize) -> usize {
        let focused = matches!(list, ListId::Screen(s, p) if s == self.nav.screen && p == self.nav.pane);
        let mut lists = self.input.lists.borrow_mut();
        let state = lists.entry(list).or_default();
        let selected = if focused { self.nav.selected } else { state.offset };
        state.window(selected, len, rows)
    }

    /// The current screen's focused list.
    pub(crate) fn main_list(&self) -> ListId {
        ListId::Screen(self.nav.screen, self.nav.pane)
    }

    /// The keyboard moved: every list follows its selection again.
    pub(crate) fn unpin_lists(&mut self) {
        for state in self.input.lists.get_mut().values_mut() {
            state.pinned = false;
        }
    }

    /// Go straight to sub-tab `i` of the current section.
    pub fn select_tab(&mut self, i: usize) {
        let section = self.nav.screen.section();
        if section == super::app::Section::Activity {
            if let Some(f) = super::app::ActivityFilter::ALL.get(i) {
                self.nav.activity_filter = *f;
                self.nav.selected = 0;
            }
            return;
        }
        if let Some(&screen) = section.screens(&self.shown()).get(i) {
            self.open_tab(screen);
        }
    }
}

#[cfg(test)]
#[path = "pointer_tests.rs"]
mod tests;
