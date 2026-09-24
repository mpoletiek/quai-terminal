//! What the verbs mean on each view, and each view's action sheet.
//!
//! One table per view ([`view_keys`]): the verbs it gives a meaning of its own (with the word
//! the footer and Help use for it here), the order the footer shows them in, and its action
//! sheet — everything else it can do, under letters that are free inside the sheet. Anything a
//! view doesn't override falls to the app's meaning of the verb (`App::app_verb`).
//!
//! The actions themselves are the views' existing handlers: a table entry names the key its
//! handler already answers (`Do::View`, `Do::Detail`), an action id (`Do::Run`), or a method
//! (`Do::Act`). Keeping that one step of indirection means the keys moved without the behaviour
//! moving with them; the per-screen modules of the next phase turn them into typed actions.

use super::super::keymap::Verb;
use super::{App, ConfirmAction, Detail, FormKind, Modal, OnboardKind, Screen};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// How a view carries out a verb or a sheet item.
#[derive(Clone, Copy)]
pub enum Do {
    /// The view's own key handler, given this key.
    View(KeyCode),
    /// The open detail view's handler, given this key.
    Detail(KeyCode),
    /// A palette action by id.
    Run(&'static str),
    /// A method.
    Act(fn(&mut App)),
    /// Open the action sheet.
    Sheet,
}

/// A verb with a meaning of its own on a view.
pub struct Override {
    pub verb: Verb,
    pub label: &'static str,
    pub how: Do,
}

/// An entry in a view's action sheet.
#[derive(Clone)]
pub struct SheetItem {
    pub key: char,
    pub label: &'static str,
    pub how: Do,
    /// Shown only while trading is on.
    pub trading: bool,
}

/// A view's keys.
pub struct ViewKeys {
    pub overrides: &'static [Override],
    /// The verbs the footer shows here, most useful first (beyond `:` and `?`).
    pub footer: &'static [Verb],
    pub sheet: &'static [SheetItem],
}

const fn ov(verb: Verb, label: &'static str, how: Do) -> Override {
    Override { verb, label, how }
}

const fn it(key: char, label: &'static str, how: Do) -> SheetItem {
    SheetItem { key, label, how, trading: false }
}

/// A sheet item that trades (buy, sell, swap): hidden while trading is off.
const fn tr(key: char, label: &'static str, how: Do) -> SheetItem {
    SheetItem { key, label, how, trading: true }
}

const fn ch(c: char) -> KeyCode {
    KeyCode::Char(c)
}

use Do::{Act, Detail as D, Run, View as V};
use Verb::*;

const HOME: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "detail", Act(App::screen_enter_pub)),
        ov(Trade, "trade", Run("trade")),
        ov(Add, "import a token", Run("import_token")),
        ov(ViewMode, "legend", V(ch('i'))),
    ],
    footer: &[Open, Trade, Send, Receive],
    sheet: &[
        it('a', "import a token", Run("import_token")),
        it('d', "discover tokens you hold", Run("discover_tokens")),
        it('i', "legend and price sources", V(ch('i'))),
        tr('t', "trade", Run("trade")),
    ],
};

const QI: ViewKeys = ViewKeys {
    overrides: &[
        ov(Send, "send Qi", Run("send_qi")),
        ov(Receive, "receive Qi", Act(App::receive_qi)),
        ov(Add, "new address", Run("new_qi_address")),
        ov(Reload, "scan", Run("scan_qi")),
    ],
    footer: &[Send, Receive, Add, Sheet],
    sheet: &[
        it('s', "scan (gap 50)", Run("scan_qi")),
        it('d', "deep scan", Run("deep_scan")),
        it('a', "aggregate small coins", Run("aggregate")),
        it('w', "sweep (keep denominations)", Run("sweep")),
        it('n', "new address", Run("new_qi_address")),
    ],
};

const ACCOUNTS: ViewKeys = ViewKeys {
    overrides: &[
        ov(Add, "add account", Run("add_account")),
        ov(Edit, "rename", Act(App::rename_account)),
        ov(Receive, "receive here", Act(App::receive_on_account)),
    ],
    footer: &[Send, Receive, Add, Edit],
    sheet: &[
        it('a', "add account", Run("add_account")),
        it('i', "import a private key", Run("import_key")),
        it('n', "new watch-only wallet", Run("new_watch_wallet")),
        it('e', "rename", Act(App::rename_account)),
    ],
};

const MARKETS: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "pairs · flow", V(KeyCode::Tab)),
        ov(PanePrev, "pairs · flow", V(KeyCode::BackTab)),
        ov(Open, "actions · chart", Act(App::markets_open)),
        ov(Trade, "trade", V(ch('t'))),
        ov(Sell, "sell to the curve", V(ch('S'))),
        ov(Flip, "flip pair", V(ch('f'))),
        ov(Sort, "sort", Act(App::cycle_market_sort)),
        ov(ViewMode, "timeframe · dust", Act(App::markets_view_mode)),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Trade, Flip, Sort, ViewMode],
    sheet: &[
        tr('t', "trade this pair", V(ch('t'))),
        it('s', "sell to the curve", V(ch('S'))),
        it('w', "watch pair", V(ch('w'))),
        it('a', "alert on pair", V(ch('A'))),
        it('f', "timeframe", V(ch('T'))),
        it('m', "hide dust in the flow", V(ch('m'))),
        it('l', "sort by TVL", V(ch('L'))),
        it('c', "sort by 24h change", V(ch('M'))),
    ],
};

const SWAP: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "edit", V(KeyCode::Tab)),
        ov(PanePrev, "edit", V(KeyCode::BackTab)),
        ov(Open, "edit · review", V(KeyCode::Enter)),
        ov(Filter, "pick token", V(ch('/'))),
        ov(Flip, "flip", V(ch('f'))),
    ],
    footer: &[Open, Filter, Flip, Sheet],
    sheet: &[
        it('m', "max", V(ch('m'))),
        it('p', "25 · 50 · 75 %", V(ch('%'))),
        it('e', "exact output", V(ch('E'))),
        it('b', "swap within bounds", V(ch('B'))),
        it('o', "create a limit order", V(ch('O'))),
        it('l', "limit orders", V(ch('L'))),
        it('s', "compare a split route", V(ch('P'))),
    ],
};

const CONVERT: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "edit", V(KeyCode::Tab)),
        ov(PanePrev, "edit", V(KeyCode::BackTab)),
        ov(Open, "quote · review", V(KeyCode::Enter)),
        ov(Filter, "pick asset", Act(App::open_exchange_picker)),
        ov(Flip, "flip", V(ch('f'))),
        ov(ViewMode, "route", V(ch('r'))),
        ov(Back, "back to swap", Act(App::exchange_back_to_swap)),
    ],
    footer: &[Open, Filter, Flip, Back, Sheet],
    sheet: &[
        tr('s', "back to a market swap", Act(App::exchange_back_to_swap)),
        it('p', "pick what to receive (any asset)", Act(App::open_exchange_picker)),
        it('m', "max", V(ch('m'))),
        it('r', "protocol or market route", V(ch('r'))),
    ],
};

const WRAP: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "edit", V(KeyCode::Tab)),
        ov(PanePrev, "edit", V(KeyCode::BackTab)),
        ov(Open, "review", V(KeyCode::Enter)),
        ov(Filter, "pick asset", Act(App::open_exchange_picker)),
        ov(Back, "back to swap", Act(App::exchange_back_to_swap)),
    ],
    footer: &[Open, Filter, Back, Sheet],
    sheet: &[
        tr('s', "back to a market swap", Act(App::exchange_back_to_swap)),
        it('p', "pick what to receive (any asset)", Act(App::open_exchange_picker)),
        it('m', "max", V(ch('m'))),
    ],
};

const POOLS: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "yours · all pools", V(KeyCode::Tab)),
        ov(PanePrev, "yours · all pools", V(KeyCode::BackTab)),
        ov(Down, "down", V(ch('j'))),
        ov(Up, "up", V(ch('k'))),
        ov(Add, "add liquidity", V(ch('a'))),
        ov(Remove, "remove liquidity", V(ch('r'))),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Add, Remove, Sheet, PaneNext],
    sheet: &[
        it('a', "add liquidity", V(ch('a'))),
        it('r', "remove liquidity", V(ch('r'))),
        it('s', "stake", V(ch('s'))),
        it('u', "unstake", V(ch('u'))),
        it('h', "harvest rewards", V(ch('h'))),
        it('e', "exit: unstake, harvest and remove", V(ch('e'))),
        it('i', "fund rewards", V(ch('i'))),
    ],
};

const LAUNCHES: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "actions", Do::Sheet),
        ov(Buy, "buy", V(ch('b'))),
        ov(Sell, "sell", V(ch('S'))),
        ov(Trade, "trade (pooled)", V(ch('t'))),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Buy, Sell, Trade, Sheet],
    sheet: &[
        it('b', "buy on the curve", V(ch('b'))),
        it('s', "sell to the curve", V(ch('S'))),
        it('c', "claim", V(ch('c'))),
        it('t', "trade where it pools", V(ch('t'))),
    ],
};

const PNL: ViewKeys = ViewKeys {
    overrides: &[ov(Trade, "trade token", V(ch('t'))), ov(Reload, "reload", V(ch('R')))],
    footer: &[Trade, Copy, Reload],
    sheet: &[it('t', "trade this token", V(ch('t')))],
};

const COLLECTED: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "detail", Act(App::screen_enter_pub)),
        ov(Left, "left", V(ch('h'))),
        ov(Right, "right", V(ch('l'))),
        ov(Down, "down", V(ch('j'))),
        ov(Up, "up", V(ch('k'))),
        ov(Send, "transfer", V(ch('T'))),
        ov(Remove, "cancel listing", V(ch('X'))),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Open, Send, Sheet],
    sheet: &[it('s', "transfer", V(ch('T'))), it('l', "list for sale", V(ch('L'))), it('x', "cancel listing", V(ch('X')))],
};

const EXPLORE: ViewKeys = ViewKeys {
    overrides: &[ov(Filter, "search", V(ch('/'))), ov(Sort, "sort", V(ch('S'))), ov(Reload, "reload", V(ch('R')))],
    footer: &[Open, Filter, Sort],
    sheet: &[],
};

const LISTINGS: ViewKeys = ViewKeys {
    overrides: &[
        ov(ViewMode, "mine · all", V(ch('m'))),
        ov(Sort, "sort", V(ch('S'))),
        ov(Buy, "buy", V(ch('b'))),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Buy, ViewMode, Sort, Sheet],
    sheet: &[
        it('b', "buy", V(ch('b'))),
        it('m', "mine · everyone's", V(ch('m'))),
        it('n', "next collection", V(ch('f'))),
        it('p', "previous collection", V(ch('F'))),
    ],
};

const CONTACTS: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "actions", Do::Sheet),
        ov(Send, "pay", Act(App::screen_enter_pub)),
        ov(Add, "add contact", Run("add_contact")),
        ov(Edit, "edit", Act(App::edit_contact)),
        ov(Remove, "remove", Act(App::remove_contact)),
    ],
    footer: &[Send, Add, Edit, Sheet],
    sheet: &[
        it('s', "pay", Act(App::screen_enter_pub)),
        it('q', "send QUAI to their address", Act(App::send_quai_to_contact)),
        it('n', "tell them your payment code", Act(App::notify_peer)),
        it('e', "edit", Act(App::edit_contact)),
        it('x', "remove", Act(App::remove_contact)),
        it('p', "add a payment channel peer", Run("add_peer")),
        it('d', "scan the mailbox", Run("discover")),
    ],
};

const CHANNELS: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "actions", Do::Sheet),
        ov(Send, "pay · accept", Act(App::screen_enter_pub)),
        ov(Add, "save as contact", Act(App::save_channel_contact_pub)),
        ov(Remove, "decline offer", Act(App::decline_offer)),
        ov(Reload, "rescan", Act(App::rescan_peer)),
    ],
    footer: &[Send, Add, Sheet],
    sheet: &[
        it('s', "pay, or accept an offer", Act(App::screen_enter_pub)),
        it('a', "save as contact", Act(App::save_channel_contact_pub)),
        it('x', "decline offer", Act(App::decline_offer)),
        it('r', "rescan this channel", Act(App::rescan_peer)),
        it('n', "tell them your payment code", Act(App::notify_peer)),
        it('p', "add a peer", Run("add_peer")),
        it('d', "scan the mailbox", Run("discover")),
    ],
};

const BOARD: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "list · messages", V(KeyCode::Tab)),
        ov(PanePrev, "list · messages", V(KeyCode::BackTab)),
        ov(Open, "write", V(ch('p'))),
        ov(Add, "follow a channel", V(ch('a'))),
        ov(Filter, "filter", V(ch('/'))),
        ov(Remove, "unfollow", Act(App::confirm_unfollow)),
        ov(Reload, "reload", V(ch('R'))),
    ],
    footer: &[Open, Add, Filter, Sheet],
    sheet: &[
        it('p', "write", V(ch('p'))),
        it('a', "follow a channel", V(ch('a'))),
        it('i', "pin beside every screen", V(ch('P'))),
        it('n', "notify me", V(ch('n'))),
        it('c', "save the sender as a contact", V(ch('c'))),
        it('x', "unfollow", Act(App::confirm_unfollow)),
    ],
};

const ACTIVITY: ViewKeys = ViewKeys {
    overrides: &[ov(Open, "detail", Act(App::screen_enter_pub))],
    footer: &[Open, Copy, CopyLink, Sheet],
    sheet: &[it('u', "speed up", Run("speedup")), it('p', "resume the trade", Act(App::resume_trade_plan))],
};

const ORDERS: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "review", Act(App::order_review)),
        ov(Remove, "cancel order", Act(App::order_cancel)),
        ov(Reload, "reload", Act(App::orders_reload)),
    ],
    footer: &[Open, Remove, Reload],
    sheet: &[
        it('r', "a fresh review, if its limit is met", Act(App::order_review)),
        it('o', "re-read it on-chain", Act(App::order_observe)),
        it('x', "cancel it", Act(App::order_cancel)),
    ],
};

const WALLETS: ViewKeys = ViewKeys {
    overrides: &[ov(Open, "actions", Do::Sheet), ov(Add, "new wallet", Act(App::new_wallet)), ov(Edit, "rename", Act(App::rename_wallet))],
    footer: &[Open, Add, Edit],
    sheet: &[
        it('o', "open this wallet (locks the current one)", Act(App::screen_enter_pub)),
        it('a', "new wallet", Act(App::new_wallet)),
        it('i', "import a recovery phrase", Act(App::import_wallet)),
        it('e', "rename", Act(App::rename_wallet)),
    ],
};

const NETWORK: ViewKeys = ViewKeys {
    overrides: &[ov(Open, "actions", Do::Sheet)],
    footer: &[Open, Reload],
    sheet: &[it('s', "switch to this network", Act(App::screen_enter_pub)), it('m', "monitoring endpoint", V(ch('m')))],
};

const SETTINGS: ViewKeys = ViewKeys {
    overrides: &[
        ov(Open, "change · open", Act(App::settings_action)),
        ov(Left, "step back", Act(App::settings_prev)),
        ov(Right, "step on", Act(App::settings_next)),
    ],
    footer: &[Open, Left, Right],
    sheet: &[],
};

const DATA_SOURCES: ViewKeys =
    ViewKeys { overrides: &[ov(Reload, "test connections", Run("test_data"))], footer: &[Open, Reload], sheet: &[] };

const ASSET: ViewKeys = ViewKeys {
    overrides: &[
        ov(Send, "send", D(ch('s'))),
        ov(Receive, "receive", D(ch('r'))),
        ov(Buy, "buy", D(ch('b'))),
        ov(Sell, "sell", D(ch('S'))),
        ov(Convert, "convert", D(ch('c'))),
        ov(Wrap, "wrap · unwrap", D(ch('w'))),
    ],
    footer: &[Send, Receive, Trade, Sheet],
    sheet: &[
        it('s', "send", D(ch('s'))),
        it('r', "receive", D(ch('r'))),
        tr('b', "buy", D(ch('b'))),
        tr('S', "sell", D(ch('S'))),
        it('c', "convert", D(ch('c'))),
        it('w', "wrap · unwrap", D(ch('w'))),
    ],
};

const NFT: ViewKeys = ViewKeys {
    overrides: &[ov(Send, "transfer", D(ch('T'))), ov(Remove, "cancel listing", D(ch('X'))), ov(Buy, "buy", D(ch('b')))],
    footer: &[Buy, Send, Sheet],
    sheet: &[
        tr('b', "buy", D(ch('b'))),
        it('s', "transfer", D(ch('T'))),
        it('l', "list for sale · change price", D(ch('L'))),
        it('x', "cancel listing", D(ch('X'))),
    ],
};

const COLLECTION: ViewKeys = ViewKeys {
    overrides: &[
        ov(PaneNext, "items · listings", D(KeyCode::Tab)),
        ov(PanePrev, "items · listings", D(KeyCode::BackTab)),
        ov(Down, "down", D(ch('j'))),
        ov(Up, "up", D(ch('k'))),
        ov(Left, "left", D(ch('h'))),
        ov(Right, "right", D(ch('l'))),
        ov(Open, "open", D(KeyCode::Enter)),
        ov(Buy, "buy", D(ch('b'))),
    ],
    footer: &[Open, Buy, PaneNext],
    sheet: &[],
};

const ACTIVITY_DETAIL: ViewKeys = ViewKeys { overrides: &[], footer: &[Copy, CopyLink], sheet: &[] };

/// The keys of the open view: its detail if one is open, else its screen.
pub fn view_keys(screen: Screen, detail: Option<&Detail>) -> &'static ViewKeys {
    match detail {
        Some(Detail::Asset(_)) => return &ASSET,
        Some(Detail::Nft(..)) => return &NFT,
        Some(Detail::Collection(_)) => return &COLLECTION,
        Some(Detail::Activity(_)) => return &ACTIVITY_DETAIL,
        None => {}
    }
    match screen {
        Screen::Home => &HOME,
        Screen::Qi => &QI,
        Screen::Accounts => &ACCOUNTS,
        Screen::Markets => &MARKETS,
        Screen::Swap => &SWAP,
        Screen::Pools => &POOLS,
        Screen::Convert => &CONVERT,
        Screen::Wrap => &WRAP,
        Screen::Launches => &LAUNCHES,
        Screen::Pnl => &PNL,
        Screen::Orders => &ORDERS,
        Screen::Collected => &COLLECTED,
        Screen::Explore => &EXPLORE,
        Screen::Listings => &LISTINGS,
        Screen::Contacts => &CONTACTS,
        Screen::Channels => &CHANNELS,
        Screen::Board => &BOARD,
        Screen::Activity => &ACTIVITY,
        Screen::Wallets => &WALLETS,
        Screen::Network => &NETWORK,
        Screen::Settings => &SETTINGS,
        Screen::DataSources => &DATA_SOURCES,
    }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

impl App {
    /// The keys of what is on screen now.
    pub(crate) fn keys_here(&self) -> &'static ViewKeys {
        view_keys(self.screen, self.detail.last())
    }

    /// Carry out one table entry.
    pub(crate) fn run_do(&mut self, how: Do) -> bool {
        match how {
            Do::View(code) => self.view_key(key(code)),
            Do::Detail(code) => self.detail_key(key(code)),
            Do::Run(id) => {
                self.run_action(id);
                true
            }
            Do::Act(f) => {
                f(self);
                true
            }
            Do::Sheet => {
                self.open_sheet();
                true
            }
        }
    }

    /// A verb on the open view: its own meaning if it has one, else the app's.
    pub(crate) fn verb(&mut self, verb: Verb, size: (u16, u16)) {
        // Tab reaches the pinned chat after the screen's last pane, whichever screen it is: this
        // comes before a screen's own Tab, or Markets and Pools would keep it to themselves.
        if verb == PaneNext && self.dock_shown && self.tab_reaches_dock() {
            self.dock_focus = true;
            return;
        }
        if let Some(o) = self.keys_here().overrides.iter().find(|o| o.verb == verb)
            && self.run_do(o.how)
        {
            return;
        }
        self.app_verb(verb, size);
    }

    /// What a verb means here, for the footer and Help: the view's word for it, or the app's.
    pub(crate) fn verb_label(&self, verb: Verb) -> &'static str {
        self.keys_here()
            .overrides
            .iter()
            .find(|o| o.verb == verb)
            .map(|o| o.label)
            .or_else(|| super::super::keymap::binding(verb).map(|b| b.label))
            .unwrap_or("")
    }

    /// The action sheet for what is focused. Empty views say so instead of opening nothing.
    pub(crate) fn open_sheet(&mut self) {
        let trading = self.config.features.trading;
        let items: Vec<SheetItem> = self.keys_here().sheet.iter().filter(|i| trading || !i.trading).cloned().collect();
        if items.is_empty() {
            self.info("no other actions here · : for everything");
            return;
        }
        self.modal = Modal::Sheet { items, selected: 0 };
    }

    /// The app's meaning of a verb, where the view has none of its own.
    pub(crate) fn app_verb(&mut self, verb: Verb, size: (u16, u16)) {
        match verb {
            Section(i) => {
                if let Some(s) = super::Section::ALL.get(i as usize).copied() {
                    self.switch_section(s);
                }
            }
            TabPrev => self.change_tab(-1),
            TabNext => self.change_tab(1),
            PaneNext | PanePrev => {
                // Tab reaches the pinned chat after the screen's last pane.
                if verb == PaneNext && self.dock_shown && self.tab_reaches_dock() {
                    self.dock_focus = true;
                    return;
                }
                let n = self.screen.panes().max(1);
                self.pane = if verb == PaneNext { (self.pane + 1) % n } else { (self.pane + n - 1) % n };
            }
            Down => self.move_selection(1),
            Up => self.move_selection(-1),
            Left | Right => {}
            Top => self.selected = 0,
            Bottom => self.selected = self.list_len().saturating_sub(1),
            PageDown => self.move_selection(10),
            PageUp => self.move_selection(-10),
            Jump => self.jump_pending = Some('\''),
            Filter => self.info("nothing to search here"),
            Open => self.screen_enter(),
            Previous => self.go_back(),
            Back => {
                if !self.detail.is_empty() {
                    self.detail.pop();
                    self.detail_selected = 0;
                    self.kitty.clear(self.caps.tmux);
                }
            }
            GoTo => self.modal = Modal::GoTo,
            Palette => self.open_palette(),
            Help => {
                self.help_scroll = 0;
                self.modal = Modal::Help;
            }
            Notifications => {
                self.modal = Modal::Notifications;
                self.send(super::Cmd::MarkRead);
            }
            Wallets => self.open_wallet_switcher(),
            Privacy => {
                self.config.balance_in_bar = !self.config.balance_in_bar;
                self.save_config();
                let on = self.config.balance_in_bar;
                self.toast(if on { "balance shown in the top bar" } else { "balance hidden ($ shows it)" }, false);
            }
            Lock => self.run_action("lock"),
            Quit => {
                self.modal = Modal::Confirm {
                    title: "Quit".into(),
                    body: "Leave quai-terminal? Background tracking stops.".into(),
                    action: ConfirmAction::Quit,
                }
            }
            Sheet => self.open_sheet(),
            Dock if self.dock_shown => self.dock_focus = true,
            Dock => self.write_pinned(),
            Send => self.run_action(if matches!(self.screen, Screen::Qi | Screen::Contacts | Screen::Channels) {
                "send_qi"
            } else {
                "send_quai"
            }),
            Receive => {
                let qi = matches!(self.screen, Screen::Qi | Screen::Contacts | Screen::Channels);
                self.modal = Modal::Receive { asset_qi: qi, account: 0 };
            }
            Trade => self.run_action("trade"),
            Buy | Sell => self.toast("buy and sell on Markets, Launches, Listings and an asset's detail", false),
            Convert => self.run_action("convert_quai_qi"),
            Wrap => self.switch(Screen::Wrap),
            Add | Edit | Remove => self.info("nothing here to change · space shows what you can do"),
            Copy => match self.selected_value() {
                Some(v) => self.copy(super::super::clipboard::PublicText::shown(v), "value"),
                None => self.toast("nothing to copy here", true),
            },
            CopyLink => self.open_link(),
            Flip | Sort | ViewMode => {}
            Reload | RefreshAll => self.send(super::Cmd::Refresh { full: true }),
        }
        let _ = size;
    }

    // ---------------------------------------------------------------- actions the tables name

    pub(crate) fn screen_enter_pub(&mut self) {
        self.screen_enter();
    }

    fn receive_qi(&mut self) {
        self.modal = Modal::Receive { asset_qi: true, account: 0 };
    }

    fn receive_on_account(&mut self) {
        let account = self.selected.min(self.dash.accounts.len().saturating_sub(1));
        self.modal = Modal::Receive { asset_qi: false, account };
    }

    fn rename_account(&mut self) {
        if let Some(a) = self.dash.accounts.get(self.selected) {
            let (addr, label) = (a.address.clone(), a.label.clone());
            self.open_form(FormKind::RenameAccount(addr));
            if let Modal::Form(f) = &mut self.modal {
                f.fields[0].value = label;
            }
        }
    }

    /// Markets `,`: every order in turn, then back to the default.
    fn cycle_market_sort(&mut self) {
        use super::super::eco::MarketSort;
        let next = match self.eco.markets_view.sort {
            MarketSort::Default => MarketSort::TvlDesc,
            MarketSort::TvlDesc => MarketSort::TvlAsc,
            MarketSort::TvlAsc => MarketSort::ChangeDesc,
            MarketSort::ChangeDesc => MarketSort::ChangeAsc,
            MarketSort::ChangeAsc => MarketSort::Default,
        };
        self.sort_markets(move |_| next);
    }

    /// Markets Enter: on the flow, the chart goes to that swap's pair; on the pairs, the pair's
    /// actions. Enter never opens a money form, and a pair on its curve would have opened a buy.
    fn markets_open(&mut self) {
        if self.pane == 1 {
            self.view_key(key(KeyCode::Enter));
        } else {
            self.open_sheet();
        }
    }

    /// Markets `.`: the chart's timeframe on the pair list, the dust floor on the flow.
    fn markets_view_mode(&mut self) {
        self.view_key(key(ch(if self.pane == 1 { 'm' } else { 'T' })));
    }

    fn edit_contact(&mut self) {
        if let Some(c) = self.dash.contacts.get(self.selected) {
            let name = c.name.clone();
            self.open_form(FormKind::Contact(Some(name)));
        }
    }

    fn remove_contact(&mut self) {
        if let Some(c) = self.dash.contacts.get(self.selected) {
            self.modal = Modal::Confirm {
                title: "Remove contact".into(),
                body: format!("Remove `{}` from your address book? Payment-channel history is kept.", c.name),
                action: ConfirmAction::RemoveContact(c.name.clone()),
            };
        }
    }

    fn send_quai_to_contact(&mut self) {
        match self.dash.contacts.get(self.selected).and_then(|c| c.address.clone()) {
            Some(address)
                if wallet_core::registry::parse_any_address(&address).is_ok_and(|a| a.ledger() == wallet_core::sdk::Ledger::Quai) =>
            {
                self.run_action("send_quai");
                if let Modal::Form(f) = &mut self.modal {
                    f.fields[1].value = address;
                    f.focus = 2;
                }
            }
            _ => self.toast("this contact has no Quai address", true),
        }
    }

    fn notify_peer(&mut self) {
        let code = if self.screen == Screen::Contacts {
            self.dash.contacts.get(self.selected).and_then(|c| c.payment_code.clone())
        } else {
            self.channel_peer().map(|p| p.code.clone())
        };
        match code {
            Some(code) if self.can_sign() => {
                self.open_form(FormKind::Notify);
                if let Modal::Form(f) = &mut self.modal {
                    f.fields[1].value = code;
                    f.focus = 1;
                }
            }
            Some(_) => self.toast("this wallet is watch-only", true),
            None => self.toast("this contact has no payment code", true),
        }
    }

    fn save_channel_contact_pub(&mut self) {
        self.save_channel_contact();
    }

    fn decline_offer(&mut self) {
        match self.channel_offer() {
            Some(o) => {
                let body = format!(
                    "Decline {} and its {} Qi? It will not be offered again; adding the code as a peer is the only way back.",
                    wallet_core::session::short_code(&o.code),
                    wallet_core::amount::qi(o.found)
                );
                self.modal =
                    Modal::Confirm { title: "Decline payment channel".into(), body, action: ConfirmAction::DeclineOffer(o.code.clone()) };
            }
            None => self.info("x declines a channel offer; registered channels stay"),
        }
    }

    fn rescan_peer(&mut self) {
        if let Some(p) = self.channel_peer() {
            self.send(super::Cmd::ScanPeer(p.code.clone()));
        }
    }

    /// Board `x`: unfollowing asks first. It used to drop the channel at once.
    fn confirm_unfollow(&mut self) {
        self.view_key(key(ch('x')));
    }

    fn new_wallet(&mut self) {
        self.begin_onboarding(OnboardKind::Create);
    }

    fn import_wallet(&mut self) {
        self.begin_onboarding(OnboardKind::ImportPhrase);
    }

    fn rename_wallet(&mut self) {
        if let Some(w) = self.wallets.get(self.selected).cloned() {
            self.open_form(FormKind::RenameWallet(w.id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action id a table names is one the palette knows: an unknown id would do nothing,
    /// silently.
    #[test]
    fn every_action_id_exists() {
        let known: Vec<&str> = super::super::ACTIONS.iter().map(|a| a.id).collect();
        let mut named: Vec<&str> = Vec::new();
        let details = [Detail::Asset("quai".into()), Detail::Nft("0x1".into(), "1".into()), Detail::Collection("0x1".into())];
        let views = Screen::ALL.iter().map(|s| view_keys(*s, None)).chain(details.iter().map(|d| view_keys(Screen::Home, Some(d))));
        for v in views {
            for how in v.overrides.iter().map(|o| o.how).chain(v.sheet.iter().map(|i| i.how)) {
                if let Do::Run(id) = how {
                    named.push(id);
                }
            }
        }
        // Actions `run_action` handles without a palette row.
        let internal = ["trade", "lock", "refresh", "themes", "lock_gallery", "mouse_release", "speedup", "scan_qi"];
        for id in named {
            assert!(known.contains(&id) || internal.contains(&id), "{id} is not an action");
        }
    }

    /// Inside one sheet, no two items share a letter; no view overrides a verb twice; every
    /// footer verb has a meaning on its view.
    #[test]
    fn every_view_table_is_consistent() {
        let details = [
            Some(Detail::Asset("quai".into())),
            Some(Detail::Nft("0x1".into(), "1".into())),
            Some(Detail::Collection("0x1".into())),
            Some(Detail::Activity("k".into())),
        ];
        let mut views: Vec<(&str, &ViewKeys)> = Screen::ALL.iter().map(|s| (s.title(), view_keys(*s, None))).collect();
        for d in &details {
            views.push(("detail", view_keys(Screen::Home, d.as_ref())));
        }
        for (name, v) in views {
            let mut letters: Vec<char> = v.sheet.iter().map(|i| i.key).collect();
            let n = letters.len();
            letters.sort_unstable();
            letters.dedup();
            assert_eq!(n, letters.len(), "{name}: two sheet items share a letter");
            let mut verbs: Vec<String> = v.overrides.iter().map(|o| format!("{:?}", o.verb)).collect();
            let n = verbs.len();
            verbs.sort();
            verbs.dedup();
            assert_eq!(n, verbs.len(), "{name}: a verb is overridden twice");
            for f in v.footer {
                assert!(super::super::super::keymap::binding(*f).is_some(), "{name}: footer verb {f:?} has no key");
            }
        }
    }
}
