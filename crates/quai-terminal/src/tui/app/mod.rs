//! TUI application state and input handling.

use super::fx::Ceremony;
use super::terminal::{Caps, KittyGraphics};
use super::theme::Theme;
use super::worker::{Cmd, Dashboard, Ev, Prepare};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::HashMap;
use std::time::Instant;
use wallet_core::appdb::OpStatus;
use wallet_core::config::{AppConfig, Feature, Features, Mode, Motion};
use wallet_core::ops::ConversionQuote;
use wallet_core::registry::{WalletKind, WalletMeta};
use wallet_core::tx::{Review, Submitted};
use zeroize::{Zeroize, Zeroizing};

#[path = "../verbs.rs"]
pub(crate) mod verbs;

/// Where a screen was left.
#[derive(Clone, Debug, Default)]
pub struct ViewState {
    pub pane: usize,
    pub selected: usize,
    /// What the cursor was on, so a list that moved meanwhile still finds it.
    pub key: Option<String>,
}

/// What the terminal shows: the feature switches, and Simple or Pro over them.
#[derive(Clone, Copy, Debug)]
pub struct Shown {
    pub features: Features,
    pub pro: bool,
}

/// Top-level sections (number keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Home,
    /// Discover and watch: pairs, launches, pools.
    Markets,
    /// Act: the exchange, orders, results.
    Trade,
    Nfts,
    People,
    Activity,
    System,
}

impl Section {
    pub const ALL: [Section; 7] =
        [Section::Home, Section::Markets, Section::Trade, Section::Nfts, Section::People, Section::Activity, Section::System];

    pub fn title(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Markets => "Markets",
            Section::Trade => "Trade",
            Section::Nfts => "NFTs",
            Section::People => "People",
            Section::Activity => "Activity",
            Section::System => "System",
        }
    }

    pub fn key(self) -> char {
        match self {
            Section::Home => '1',
            Section::Markets => '2',
            Section::Trade => '3',
            Section::Nfts => '4',
            Section::People => '5',
            Section::Activity => '6',
            Section::System => '0',
        }
    }

    /// Every view behind the sub-tabs, whether its feature is on or not (Activity has one view
    /// with filter tabs).
    pub fn all_screens(self) -> &'static [Screen] {
        match self {
            Section::Home => &[Screen::Home, Screen::Qi, Screen::Accounts],
            Section::Markets => &[Screen::Markets, Screen::Launches, Screen::Pools],
            Section::Trade => &[Screen::Exchange, Screen::Orders, Screen::Pnl],
            Section::Nfts => &[Screen::Collected, Screen::Explore, Screen::Listings],
            Section::People => &[Screen::Contacts, Screen::Board],
            Section::Activity => &[Screen::Activity],
            Section::System => &[Screen::Wallets, Screen::Network, Screen::Settings, Screen::DataSources],
        }
    }

    /// The sub-tabs shown, one view each. A section whose views are all turned off, or all
    /// Pro while the terminal is Simple, is hidden altogether.
    pub fn screens(self, shown: &Shown) -> Vec<Screen> {
        let mut tabs: Vec<Screen> = Vec::new();
        for s in self.all_screens().iter().copied().filter(|s| s.enabled(shown)) {
            if !tabs.contains(&s) {
                tabs.push(s);
            }
        }
        tabs
    }

    /// Sub-tab labels.
    pub fn tab_labels(self, shown: &Shown) -> Vec<&'static str> {
        match self {
            Section::Activity => ActivityFilter::ALL.iter().map(|f| f.title()).collect(),
            _ => self.screens(shown).iter().map(|s| s.tab_title()).collect(),
        }
    }
}

/// An activity row: (time, is an operation, index into ops or activity).
pub type ActivityRow = (u64, bool, usize);

/// Activity filter tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ActivityFilter {
    #[default]
    All,
    Sends,
    Receipts,
    Trades,
    Nfts,
}

impl ActivityFilter {
    pub const ALL: [ActivityFilter; 5] =
        [ActivityFilter::All, ActivityFilter::Sends, ActivityFilter::Receipts, ActivityFilter::Trades, ActivityFilter::Nfts];
    pub fn title(self) -> &'static str {
        match self {
            ActivityFilter::All => "All",
            ActivityFilter::Sends => "Sends",
            ActivityFilter::Receipts => "Receipts",
            ActivityFilter::Trades => "Trades",
            ActivityFilter::Nfts => "NFTs",
        }
    }
}

/// Views (a section's sub-tabs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Screen {
    Home,
    Qi,
    /// Accounts, with the wallet's time locks beneath them.
    Accounts,
    Markets,
    /// Swap, convert and wrap: one screen, whose card the pair decides ([`Card`]).
    Exchange,
    /// Liquidity positions, adding, removing and gauge staking.
    Pools,
    /// Limit orders: watch, review, cancel.
    Orders,
    /// Quainance's launch zone: bonding-curve launches and where they trade now.
    Launches,
    /// Trading performance in QUAI from this wallet's own trades.
    Pnl,
    Collected,
    Explore,
    Listings,
    /// Contacts, with the payment channels to them in a second pane.
    Contacts,
    /// The on-chain message board.
    Board,
    /// Wallets on this computer: switch, create, import.
    Wallets,
    Activity,
    Network,
    Settings,
    DataSources,
}

impl Screen {
    pub const ALL: [Screen; 19] = [
        Screen::Home,
        Screen::Qi,
        Screen::Accounts,
        Screen::Markets,
        Screen::Exchange,
        Screen::Pools,
        Screen::Orders,
        Screen::Launches,
        Screen::Pnl,
        Screen::Collected,
        Screen::Explore,
        Screen::Listings,
        Screen::Contacts,
        Screen::Board,
        Screen::Wallets,
        Screen::Activity,
        Screen::Network,
        Screen::Settings,
        Screen::DataSources,
    ];

    /// The impl that answers for this screen ([`super::screen`]).
    pub fn view(self) -> &'static dyn super::screen::ScreenView {
        super::screen::view(self)
    }

    pub fn title(self) -> &'static str {
        self.view().title()
    }

    /// Where this view is, in words and keys: "Activity (5)", "Trade › Convert (2)". Computed
    /// from the section table, so it can't drift from the keys the way hand-written paths did.
    pub fn place(self) -> String {
        let section = self.section();
        let chord = super::keymap::chord(self);
        if section.all_screens().len() == 1 {
            format!("{} ({chord})", section.title())
        } else {
            format!("{} › {} ({chord})", section.title(), self.title())
        }
    }

    /// How the tab strip and the header name this view.
    pub fn tab_title(self) -> &'static str {
        self.title()
    }

    pub fn section(self) -> Section {
        Section::ALL.iter().copied().find(|s| s.all_screens().contains(&self)).unwrap_or(Section::Home)
    }

    /// The optional feature this view belongs to.
    pub fn feature(self) -> Option<Feature> {
        self.view().feature()
    }

    /// Part of Pro: hidden while the terminal is Simple (U4 in the architecture review).
    pub fn pro_only(self) -> bool {
        self.view().pro_only()
    }

    /// Shown: its feature is on, and it is not Pro in a Simple terminal.
    pub fn enabled(self, shown: &Shown) -> bool {
        self.feature().is_none_or(|f| shown.features.on(f)) && (shown.pro || !self.pro_only())
    }

    /// Panes that Tab / Shift-Tab move focus between.
    pub fn panes(self) -> usize {
        self.view().panes()
    }
}

/// The exchange's cards: what the pair makes it. QUAI and Qi convert; Qi and WQI, or QUAI and
/// WQUAI, wrap; anything else swaps.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Card {
    #[default]
    Swap,
    Convert,
    Wrap,
}

impl Card {
    pub const ALL: [Card; 3] = [Card::Swap, Card::Convert, Card::Wrap];

    pub fn title(self) -> &'static str {
        match self {
            Card::Swap => "Swap",
            Card::Convert => "Convert",
            Card::Wrap => "Wrap",
        }
    }

    /// Where this card is, in words and keys: "Trade › Exchange › Wrap (g w)".
    pub fn place(self) -> String {
        format!("Trade › Exchange › {} ({})", self.title(), super::keymap::chord_to(super::keymap::Place::Card(self)))
    }
}

/// The footer: the keys that act on whatever has the keyboard, most useful first, never more
/// than [`FOOTER_HINTS`] (then `space` for everything else here, `:` and `?`). It follows the top
/// layer: an open modal, the pinned chat, a focused field, or the view's own table.
pub fn context_hints(app: &App) -> Vec<(String, String)> {
    let pair = |k: &str, v: &str| (k.to_string(), v.to_string());
    match &app.modal {
        Modal::None => {}
        Modal::Review(r) => {
            return if r.can_approve() {
                vec![
                    pair("tab", "reject · approve"),
                    pair("enter", "the focused button"),
                    pair("y", "copy as a command"),
                    pair("esc", "reject"),
                ]
            } else {
                vec![pair("j/space", "read on"), pair("esc", "reject")]
            };
        }
        Modal::Form(_) => return vec![pair("tab", "next field"), pair("←→", "choices"), pair("enter", "continue"), pair("esc", "cancel")],
        Modal::Sheet { .. } => return vec![pair("letter", "do it"), pair("↑↓ enter", "choose"), pair("esc", "close")],
        Modal::GoTo => return vec![pair("letter", "go"), pair("g", "first row"), pair("esc", "stay")],
        Modal::Wallets { .. } => return vec![pair("enter", "switch"), pair("m", "manage"), pair("esc", "close")],
        Modal::Accounts { .. } => return vec![pair("enter or 1-9", "act from it"), pair("esc", "close")],
        Modal::Confirm { .. } => return vec![pair("n enter", "no"), pair("y", "yes")],
        Modal::Palette { .. } => {
            return vec![pair("type", "search"), pair("↑↓ enter", "run"), pair("ctrl-y", "copy command"), pair("esc", "close")];
        }
        Modal::Help => return vec![pair("j/k", "scroll"), pair("g", "glossary"), pair("esc", "close")],
        _ => return vec![pair("esc", "close")],
    }
    if app.dock.focus {
        return vec![
            pair("enter", "post · review"),
            pair("tab", "back to the screen"),
            pair("esc", "leave, keep draft"),
            pair("ctrl-u", "clear"),
        ];
    }
    if app.nav.detail.is_empty() && app.input_focused() {
        return match (app.nav.screen, app.nav.card) {
            (Screen::Exchange, Card::Swap) => match app.eco.swap.field {
                1 if app.swap_quote_current() && app.eco.swap.quote.as_ref().is_some_and(|q| q.is_ok()) => {
                    vec![pair("enter", "review"), pair("m", "max"), pair("%", "share"), pair("esc", "done")]
                }
                1 => vec![pair("0-9", "amount"), pair("m", "max"), pair("%", "25·50·75%"), pair("esc", "done")],
                0 | 2 => vec![pair("enter", "pick token"), pair("f", "flip"), pair("tab", "next"), pair("esc", "done")],
                _ => vec![pair("←→", "adjust"), pair("tab", "next"), pair("esc", "done")],
            },
            (Screen::Exchange, _) => {
                vec![pair("0-9", "amount"), pair("←→", "adjust"), pair("enter", "continue"), pair("esc", "done")]
            }
            (Screen::Pools, _) => vec![pair("0-9", "amount"), pair("tab", "next"), pair("enter", "review"), pair("esc", "cancel")],
            _ => vec![pair("type", "search"), pair("enter", "done"), pair("esc", "clear")],
        };
    }
    let keys = app.keys_here();
    let mut out: Vec<(String, String)> = keys
        .footer
        .iter()
        // The sheet has its own place at the end of the footer.
        .filter(|v| **v != super::keymap::Verb::Sheet)
        .filter(|v| {
            app.config.features.trading || !matches!(v, super::keymap::Verb::Trade | super::keymap::Verb::Buy | super::keymap::Verb::Sell)
        })
        .map(|v| (super::keymap::key_of(*v), app.verb_label(*v).to_string()))
        .collect();
    out.truncate(FOOTER_HINTS);
    if !app.nav.detail.is_empty() {
        out.truncate(FOOTER_HINTS - 1);
        out.push(pair("esc", "back"));
    }
    out
}

/// Screen hints in the footer at once; with `:` and `?` that is six keys to read, not twelve.
pub const FOOTER_HINTS: usize = 4;

/// What the detail stack can show (Enter pushes, Esc pops).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detail {
    /// A portfolio row (`quai`, `qi` or token address).
    Asset(String),
    /// An NFT (contract, token id).
    Nft(String, String),
    /// A collection (contract).
    Collection(String),
    /// An activity row key (`op:<id>` or `act:<network key>`).
    Activity(String),
}

/// How a form field accepts input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    /// Hidden; never echoed.
    Secret,
    /// Hidden with a strength meter (new passwords).
    NewSecret,
    /// Decimal amount of an asset (`QUAI`, `QI`, `WQI`, `WQUAI`, `TOKEN`).
    Amount(&'static str),
    /// Cycled with ←/→: (value, label).
    Choice(Vec<(String, String)>),
}

#[derive(Clone, Debug)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub kind: FieldKind,
    pub hint: String,
    pub optional: bool,
}

impl Field {
    pub fn new(label: &str, hint: &str) -> Self {
        Self { label: label.into(), value: String::new(), kind: FieldKind::Text, hint: hint.into(), optional: false }
    }
    pub fn with(mut self, value: impl Into<String>) -> Self {
        self.value = value.into();
        self
    }
    pub fn secret(mut self) -> Self {
        self.kind = FieldKind::Secret;
        self
    }
    pub fn new_secret(mut self) -> Self {
        self.kind = FieldKind::NewSecret;
        self
    }
    pub fn amount(mut self, asset: &'static str) -> Self {
        self.kind = FieldKind::Amount(asset);
        self
    }
    pub fn choice(mut self, options: Vec<(String, String)>) -> Self {
        if self.value.is_empty()
            && let Some((v, _)) = options.first()
        {
            self.value = v.clone();
        }
        self.kind = FieldKind::Choice(options);
        self
    }
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }
    pub fn is_secret(&self) -> bool {
        matches!(self.kind, FieldKind::Secret | FieldKind::NewSecret)
    }
    /// Display label of a choice value.
    pub fn choice_label(&self) -> Option<&str> {
        match &self.kind {
            FieldKind::Choice(opts) => opts.iter().find(|(v, _)| *v == self.value).map(|(_, l)| l.as_str()),
            _ => None,
        }
    }
    fn cycle(&mut self, delta: i32) {
        if let FieldKind::Choice(opts) = &self.kind
            && !opts.is_empty()
        {
            let i = opts.iter().position(|(v, _)| *v == self.value).unwrap_or(0) as i32;
            self.value = opts[(i + delta).rem_euclid(opts.len() as i32) as usize].0.clone();
        }
    }
}

impl FormKind {
    /// A form whose text is a private message: dropped at lock rather than kept for later.
    pub fn is_private(&self) -> bool {
        matches!(self, FormKind::Message { .. } | FormKind::MessageNew)
    }
}

/// Form purposes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormKind {
    BoundedSwap {
        from: String,
        to: String,
        input: String,
    },
    StakePosition {
        pair: String,
        gauge: Option<String>,
        name: String,
        amount: String,
        stake: bool,
    },
    OrderCreate {
        from: String,
        to: String,
        input: String,
        slippage: u16,
        /// The Swap card's quote when the form opened, for the live preview.
        preview: Box<super::order_ui::Preview>,
    },
    ExactOutput {
        from: String,
        to: String,
    },
    /// This wallet's password, for the daemon (checked hand-off; nothing is kept).
    DaemonUnlock,
    /// An alert on a market pair.
    Alert {
        pool: String,
        name: String,
        inverted: bool,
    },
    SendQuai,
    SendQi,
    SendToken,
    Approve,
    ConvertQuaiToQi,
    ConvertQiToQuai,
    /// Withdraw a percentage of a position.
    RemoveLiquidity {
        pair: String,
        name: String,
    },
    /// Fund a pool's gauge rewards for everyone staking in it.
    Incentivize {
        pair: String,
        name: String,
    },
    /// Buy a token on its bonding curve.
    CurveBuy {
        token: String,
        symbol: String,
        curve: String,
    },
    /// Sell a token back to its bonding curve.
    CurveSell {
        token: String,
        symbol: String,
        curve: String,
        held: String,
    },
    WrapQi,
    UnwrapWqi,
    WrapQuai,
    UnwrapQuai,
    Notify,
    /// Post to a channel of the message board.
    BoardPost {
        channel: String,
    },
    /// Send a private message (v3) to a messaging address.
    Message {
        peer: String,
        name: Option<String>,
    },
    /// A private message to someone not in the list yet: address or contact, and the text.
    MessageNew,
    /// Follow another channel.
    FollowChannel,
    /// Call a function on a contract, chosen from the ABI the contract publishes itself.
    ContractCall {
        address: String,
        /// The contract's name from its metadata, for the title.
        name: String,
        /// The functions that can be signed for, in the order they are offered.
        functions: Vec<wallet_core::contracts::Callable>,
    },
    /// Where IPFS content of one kind is fetched from (Settings).
    IpfsGateway(wallet_core::ipfs::Content),
    AddAccount,
    /// A private key into this wallet: another signing account beside its own.
    ImportKey,
    /// Another address for this watch-only wallet to follow.
    WatchAddress,
    RenameAccount(String),
    NewQiAddress,
    /// Add (None) or edit (Some(original name)) a contact.
    Contact(Option<String>),
    /// Save a Quai account or a payment code to a contact: one already saved (the choice), or a
    /// new one. `value` fills its field; `contact` is chosen first when given.
    SaveToContact {
        value: Option<String>,
        contact: Option<String>,
    },
    ImportToken,
    DeepScan,
    ExportPhrase,
    Backup,
    Quote,
    /// Rename a wallet by id.
    RenameWallet(String),
    /// Set or clear a network's read-only monitoring endpoint.
    Monitor {
        network: String,
    },
    /// List an item for sale, or change the price of this wallet's listing (`current`).
    NftList {
        contract: String,
        token_id: String,
        owner: String,
        name: String,
        current: Option<(String, String)>,
    },
    /// Transfer an NFT (ERC-1155 asks for a quantity).
    NftTransfer {
        contract: String,
        token_id: String,
        multi: bool,
    },
}

impl FormKind {
    /// Whether this form finishes on this computer: nothing is prepared, signed or sent, so it
    /// closes the moment it is submitted. Every other form waits for the worker to answer, and a
    /// local one left waiting sits on "preparing…" for a review that never comes.
    pub fn is_local(&self) -> bool {
        matches!(
            self,
            FormKind::Monitor { .. }
                | FormKind::RenameWallet(_)
                | FormKind::FollowChannel
                | FormKind::IpfsGateway(_)
                | FormKind::Alert { .. }
                | FormKind::DaemonUnlock
        )
    }
}

#[derive(Clone, Debug)]
pub struct Form {
    pub kind: FormKind,
    pub title: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub note: Option<String>,
    /// What the destination turned out to be, when the chain has answered. Kept apart from
    /// `note`, which is written when the form opens and says what the form is for.
    pub contract_note: Option<String>,
    /// Submitted and waiting for the worker; the form stays open so errors can be fixed in place.
    pub pending: bool,
    pub error: Option<String>,
    pub error_field: Option<usize>,
}

impl Drop for Form {
    fn drop(&mut self) {
        for f in &mut self.fields {
            if f.is_secret() {
                f.value.zeroize();
            }
        }
    }
}

/// Hold to sign: how long Enter is held, and the longest pause between its repeats (the first
/// repeat comes after the keyboard's delay, up to about 600 ms).
pub const HOLD_TO_SIGN: std::time::Duration = std::time::Duration::from_millis(1000);
pub const HOLD_GAP: std::time::Duration = std::time::Duration::from_millis(650);

/// Switches closer together than this skip the border draw-in.
pub const FAST_HANDS: std::time::Duration = std::time::Duration::from_millis(400);

/// How long a mined transaction is said where its pending pill was.
pub const PILL_RESOLVED: std::time::Duration = std::time::Duration::from_millis(2500);

/// Review confirmation state.
pub struct ReviewState {
    pub review: Review,
    pub scroll: u16,
    pub content_lines: u16,
    pub viewport: u16,
    pub approve_focused: bool,
    pub opened: Instant,
    /// What has been typed toward a risky review's confirmation words.
    pub typed: String,
}

impl ReviewState {
    /// Approval enables only once the whole review has been scrolled into view, shortly after
    /// opening, and — for a risky review — once its confirmation words have been typed exactly.
    pub fn can_approve(&self) -> bool {
        let seen_all = self.scroll + self.viewport >= self.content_lines;
        seen_all && self.opened.elapsed().as_millis() > 400 && self.words_typed()
    }

    /// Whether the review asks for no words, or they have been typed exactly.
    pub fn words_typed(&self) -> bool {
        self.review.confirm.as_deref().is_none_or(|phrase| self.typed.trim() == phrase)
    }
    /// Fraction of the review that has been in view.
    pub fn read_ratio(&self) -> f64 {
        if self.content_lines == 0 {
            return 1.0;
        }
        (f64::from(self.scroll + self.viewport) / f64::from(self.content_lines)).min(1.0)
    }
}

/// A theme candidate in the showroom.
pub struct PickerEntry {
    pub id: String,
    pub name: String,
    pub family: String,
    pub theme: Theme,
}

/// Theme showroom: live preview while moving, revert on cancel.
pub struct Picker {
    pub entries: Vec<PickerEntry>,
    pub selected: usize,
    pub filter: String,
    pub original: Theme,
}

pub enum PickerOutcome {
    Open,
    Applied,
    Cancelled,
}

impl Picker {
    pub fn new(app: &App) -> Picker {
        let mut entries = Vec::new();
        let resolve = |id: &str| {
            let mut t = super::theme::resolve(app.paths.root(), id, app.term.light_hint, app.term.no_color).0;
            t.fit_to_terminal(app.term.background, app.term.caps.ansi8, app.term.caps.truecolor);
            t
        };
        entries.push(PickerEntry {
            id: "auto".into(),
            name: if super::theme::omarchy_colors().is_some() { "Omarchy (live)".into() } else { "Auto".into() },
            family: "Follow system".into(),
            theme: resolve("auto"),
        });
        entries.push(PickerEntry {
            id: "terminal".into(),
            name: "Terminal palette".into(),
            family: "Follow system".into(),
            theme: resolve("terminal"),
        });
        entries.push(PickerEntry {
            id: "monochrome".into(),
            name: "Monochrome".into(),
            family: "Accessibility".into(),
            theme: resolve("monochrome"),
        });
        for t in super::themes::CATALOG {
            entries.push(PickerEntry { id: t.id.into(), name: t.name.into(), family: t.family.into(), theme: resolve(t.id) });
        }
        for (id, source) in super::theme::available(app.paths.root()) {
            if !entries.iter().any(|e| e.id == id) {
                entries.push(PickerEntry {
                    id: id.clone(),
                    name: id.clone(),
                    family: format!("Yours · {}", short_path(&source)),
                    theme: resolve(&id),
                });
            }
        }
        let selected = entries.iter().position(|e| e.id == app.config.theme).unwrap_or(0);
        Picker { entries, selected, filter: String::new(), original: app.theme.clone() }
    }

    /// Indexes matching the filter.
    pub fn visible(&self) -> Vec<usize> {
        let fold = |s: &str| s.to_lowercase().replace(['é', 'è', 'ê'], "e");
        let f = fold(&self.filter);
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                f.is_empty() || fold(&e.name).contains(&f) || fold(&e.family).contains(&f) || e.id.contains(&f)
            })
            .collect()
    }

    pub fn current(&self) -> Option<&PickerEntry> {
        self.entries.get(self.selected)
    }

    /// Shared key handling for onboarding and settings. Updates `theme` live.
    pub fn on_key(&mut self, key: KeyEvent, theme: &mut Theme) -> PickerOutcome {
        let visible = self.visible();
        let pos = visible.iter().position(|&i| i == self.selected);
        let step = |delta: i32| -> Option<usize> {
            if visible.is_empty() {
                return None;
            }
            let p = pos.map(|p| p as i32).unwrap_or(-1);
            Some(visible[(p + delta).rem_euclid(visible.len() as i32) as usize])
        };
        match key.code {
            KeyCode::Esc => {
                *theme = self.original.clone();
                return PickerOutcome::Cancelled;
            }
            KeyCode::Enter => return if self.current().is_some() { PickerOutcome::Applied } else { PickerOutcome::Open },
            KeyCode::Down | KeyCode::Tab => self.selected = step(1).unwrap_or(self.selected),
            KeyCode::Up | KeyCode::BackTab => self.selected = step(-1).unwrap_or(self.selected),
            KeyCode::PageDown => self.selected = step(8).unwrap_or(self.selected),
            KeyCode::PageUp => self.selected = step(-8).unwrap_or(self.selected),
            KeyCode::Backspace => {
                self.filter.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter.push(c);
                if let Some(&first) = self.visible().first()
                    && !self.visible().contains(&self.selected)
                {
                    self.selected = first;
                }
            }
            _ => {}
        }
        if let Some(e) = self.current() {
            *theme = e.theme.clone();
        }
        PickerOutcome::Open
    }
}

/// Lock screen animation gallery: `random` plus every effect, with a live looping preview.
pub struct Gallery {
    /// 0 = random, n = `fx::EFFECTS[n - 1]`.
    pub selected: usize,
    pub preview: Option<Ceremony>,
}

impl Gallery {
    pub fn new(current: &str) -> Gallery {
        let selected = super::fx::EFFECTS.iter().position(|(n, _)| *n == current).map_or(0, |i| i + 1);
        Gallery { selected, preview: None }
    }
    /// Effect name for the selection (`random` previews a rotating pick).
    pub fn value(&self) -> &'static str {
        if self.selected == 0 { "random" } else { super::fx::EFFECTS[self.selected - 1].0 }
    }
}

/// Size of the lock screen art area for a terminal size (shared with the renderer).
pub fn lock_art_size((w, h): (u16, u16)) -> (u16, u16) {
    (w, h.saturating_sub(11).max(6))
}

/// Outcome of verifying a monitoring endpoint: network id, endpoint, result.
pub type MonitorCheck = (String, wallet_core::network::MonitorEndpoint, Result<(), String>);
pub type IpfsCheck = (wallet_core::ipfs::Content, wallet_core::ipfs::Gateway, Result<wallet_core::ipfs::TestOutcome, String>);

/// What a yes/no confirmation does.
#[derive(Clone, Debug)]
pub enum ConfirmAction {
    Quit,
    RemoveContact(String),
    /// Save to a contact what replaces or moves something saved: (contact, value).
    SaveToContact(String, String),
    SwitchNetwork(String),
    /// Register an announced sender's channel.
    AcceptOffer(String),
    /// Decline an announced sender's channel, for good.
    DeclineOffer(String),
    /// Stop following a Board channel.
    Unfollow(String),
    /// Drop a messaging address's messages unread from now on.
    BlockPeer(String),
    /// Accept a peer's new identity key.
    TrustPeer(String),
    /// Record that a peer's fingerprint matched.
    VerifyPeer(String),
}

pub enum Modal {
    None,
    /// The wallet switcher (`W`, or a click on the wallet name).
    Wallets {
        selected: usize,
    },
    /// The account that acts (`@`, or a click on the account in the header).
    Accounts {
        selected: usize,
    },
    Form(Form),
    Review(ReviewState),
    Help,
    /// What the wallet's words mean, one term open (`glossary::TERMS` index).
    Glossary {
        selected: usize,
    },
    Palette {
        query: String,
        selected: usize,
    },
    Receive {
        asset_qi: bool,
        account: usize,
    },
    Secret {
        text: Zeroizing<String>,
        title: String,
    },
    Quote(Box<ConversionQuote>),
    Result(Submitted),
    Notifications,
    /// The action sheet: everything the focused thing can do, under its own letters.
    Sheet {
        items: Vec<verbs::SheetItem>,
        selected: usize,
    },
    /// `g` pressed: where it can go, one letter each.
    GoTo,
    /// Something the user must read before going on: a failed send, a refusal. Enter or Esc.
    Notice {
        title: String,
        body: Vec<String>,
        error: bool,
        /// A reference to keep (an operation id), shown dim under the body.
        detail: Option<String>,
    },
    Confirm {
        title: String,
        body: String,
        action: ConfirmAction,
    },
    Themes(Picker),
    Effects(Gallery),
    /// Swap token picker (pay or receive side).
    TokenPicker {
        pay: bool,
        query: String,
        selected: usize,
    },
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub text: String,
    pub level: Severity,
    pub at: Instant,
    /// Names a toast the app updates or takes back itself (the auto-lock countdown).
    pub id: Option<&'static str>,
}

/// How much a toast matters: its mark, its color and how long it stays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Info,
    Ok,
    Attention,
    Danger,
}

impl Toast {
    /// Seconds on screen: an error stays long enough to be read twice; a warning with an id
    /// stays until the app takes it back.
    fn lasts(&self) -> u64 {
        match (self.level, self.id) {
            (_, Some(_)) => 120,
            (Severity::Danger, _) => 15,
            (Severity::Attention, _) => 10,
            _ => 6,
        }
    }
}

/// Onboarding steps when no wallets exist.
pub enum Onboarding {
    /// The first thing a new person sees: what this is, before any question.
    Welcome,
    Theme(Picker),
    /// How much moves. Each choice previews live; `from` is what esc puts back.
    Motion {
        selected: usize,
        from: Motion,
    },
    /// Whether address-linked explorer lookups are on, asked before any wallet exists.
    Privacy {
        selected: usize,
    },
    /// Where this wallet reads from: a monitoring node of your own, and the two IPFS gateways.
    /// Every field has a working default, so enter carries straight through.
    Connections {
        fields: Vec<Field>,
        focus: usize,
    },
    Choose {
        selected: usize,
    },
    ShowPhrase {
        phrase: Zeroizing<String>,
    },
    Quiz {
        phrase: Zeroizing<String>,
        indexes: [usize; 3],
        answers: [String; 3],
        focus: usize,
    },
    Details {
        kind: OnboardKind,
        phrase: Option<Zeroizing<String>>,
        fields: Vec<Field>,
        focus: usize,
        verified: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnboardKind {
    Create,
    ImportPhrase,
    ImportKey,
    Watch,
}

/// Background wallet creation (Argon2 runs off the render thread).
pub type Creation = std::sync::mpsc::Receiver<Result<(WalletMeta, Option<Zeroizing<String>>), String>>;

/// Palette entries. The keys that reach each one are not written here: the palette works them
/// out from the keymap (`palette::keys_for`), so they can't drift from what the keys do.
#[derive(Clone, Debug)]
pub struct Action {
    pub label: &'static str,
    pub cli: &'static str,
    pub id: &'static str,
}

macro_rules! action {
    ($label:literal, $cli:literal, $id:literal) => {
        Action { label: $label, cli: $cli, id: $id }
    };
}

pub const ACTIONS: &[Action] = &[
    action!("Switch account (the one that acts)", "quai-terminal account use N", "switch_account"),
    action!("Send QUAI", "quai-terminal send quai --to ADDR --amount N", "send_quai"),
    action!("Send Qi", "quai-terminal send qi --to CODE --amount N", "send_qi"),
    action!("Send token", "quai-terminal send token SYMBOL --to ADDR --amount N", "send_token"),
    action!("Receive QUAI", "quai-terminal receive --qr", "receive_quai"),
    action!("Receive Qi (payment code)", "quai-terminal receive --asset qi --qr", "receive_qi"),
    action!("Portfolio (tokens, prices, value)", "quai-terminal portfolio", "portfolio"),
    action!("Home", "quai-terminal portfolio --history", "home"),
    action!("Swap tokens (Quainance)", "quai-terminal swap FROM TO --amount N", "swap"),
    action!("Convert QUAI → Qi", "quai-terminal convert quai-to-qi --amount N", "convert_quai_qi"),
    action!("Convert Qi → QUAI", "quai-terminal convert qi-to-quai --amount N", "convert_qi_quai"),
    action!("Conversion quote & risk", "quai-terminal convert quote quai-to-qi N", "quote"),
    action!("Wrap Qi → WQI", "quai-terminal wrap qi --amount N", "wrap_qi"),
    action!("Claim WQI", "quai-terminal wrap claim", "claim_wqi"),
    action!("Unwrap WQI → Qi", "quai-terminal wrap unwrap-qi --amount N", "unwrap_wqi"),
    action!("Wrap QUAI → WQUAI", "quai-terminal wrap quai --amount N", "wrap_quai"),
    action!("Unwrap WQUAI → QUAI", "quai-terminal wrap unwrap-quai --amount N", "unwrap_quai"),
    action!("NFTs you hold", "quai-terminal nft list", "nfts"),
    action!("Explore NFT collections", "quai-terminal market collections", "explore"),
    action!("NFT listings (Bazarr)", "quai-terminal market listings", "listings"),
    action!("Approve token spender", "quai-terminal token approve SYMBOL SPENDER --amount N", "approve"),
    action!("Import token", "quai-terminal token import ADDRESS", "import_token"),
    action!("Discover tokens you hold", "quai-terminal token discover --import", "discover_tokens"),
    action!("Add Quai account", "quai-terminal account add", "add_account"),
    action!("Import a private key as an account", "quai-terminal wallet import-key", "import_key"),
    action!("Watch an address (watch-only wallet)", "quai-terminal account watch ADDRESS", "watch_address"),
    action!("New watch-only wallet", "quai-terminal wallet watch --name NAME ADDRESS", "new_watch_wallet"),
    action!("New Qi / mining address", "quai-terminal mining new", "new_qi_address"),
    action!("Scan Qi (gap 50)", "quai-terminal qi scan", "scan_qi"),
    action!("Deep scan Qi", "quai-terminal qi scan --deep N", "deep_scan"),
    action!("Consolidate Qi (aggregate small coins)", "quai-terminal qi consolidate --aggregate", "aggregate"),
    action!("Sweep Qi (keep denominations)", "quai-terminal qi consolidate", "sweep"),
    action!("Time locks", "quai-terminal locks", "locks"),
    action!("Discover payments (mailbox)", "quai-terminal payment discover", "discover"),
    action!("Add payment peer", "quai-terminal payment add CODE", "add_peer"),
    action!("Notify payment peer", "quai-terminal payment notify PEER", "notify"),
    action!("Contacts (addresses & payment codes)", "quai-terminal contact list", "contacts"),
    action!("Add contact", "quai-terminal contact add NAME --address ADDR", "add_contact"),
    action!("Launches (Quainance launch zone)", "quai-terminal pool launches", "launches"),
    action!("Trading PnL in QUAI", "quai-terminal pnl --trades", "pnl"),
    action!("Speed up selected transaction", "quai-terminal tx speedup ID", "speedup"),
    action!("Fill nonce gap", "quai-terminal tx fill-gap --from ACCOUNT", "fill_gap"),
    action!("Data sources", "quai-terminal data status", "data_sources"),
    action!("Test data connections", "quai-terminal data test", "test_data"),
    action!("Theme showroom", "quai-terminal theme list", "themes"),
    action!("Pro: every screen (markets, pools, orders, the board…)", "quai-terminal config set mode pro", "pro"),
    action!("Simple: the everyday screens", "quai-terminal config set mode simple", "simple"),
    action!("Release the mouse (select text with the terminal)", "quai-terminal config set mouse off", "mouse_release"),
    action!("Glossary: what the words mean", "", "glossary"),
    action!("Unlock this wallet in the daemon", "quai-terminal daemon unlock -w NAME", "daemon_unlock"),
    action!("Lock screen gallery", "quai-terminal config set lock_effect NAME", "lock_gallery"),
    action!("Refresh everything", "quai-terminal balance", "refresh"),
    action!("Lock wallet", "", "lock"),
    action!("Reveal recovery phrase", "quai-terminal wallet export-mnemonic", "export_phrase"),
    action!("Encrypted backup", "quai-terminal wallet backup FILE", "backup"),
    action!("Switch network", "quai-terminal network use ID", "network"),
    action!("Notifications", "quai-terminal notifications", "notifications"),
    action!("Enter the matrix", "", "matrix"),
    action!("PoEM · proof of entropy minima (:poem)", "", "poem"),
    action!("Help", "quai-terminal --help", "help"),
    action!("Quit", "", "quit"),
];

/// The chat docked beside every screen: shown, focused, and the draft.
pub struct Dock {
    /// The pinned chat was drawn this frame, so Tab can reach it.
    pub shown: bool,
    /// The pinned chat has the keyboard: typing goes into its message box.
    pub focus: bool,
    /// What is being written in the pinned chat, kept while focus is elsewhere.
    pub draft: String,
}

/// Every wallet on this computer: the list, each one's last summary and its live QUAI.
pub struct Cockpit {
    /// Wallets on this computer, for the Wallets screen (re-read when it opens).
    pub list: Vec<WalletMeta>,
    /// Each wallet's last priced summary on this network (System › Wallets).
    pub summaries: HashMap<String, wallet_core::cockpit::WalletSummary>,
    /// Each wallet's QUAI read live from its public addresses.
    pub quai: HashMap<String, wallet_core::sdk::U256>,
}

/// What arrived and how it was said: unseen arrivals, the first payment, the bell.
pub struct News {
    /// Money arrived since Activity was last opened: the rail marks Activity until it is. The
    /// static half of the arrival, kept at every motion level.
    pub arrivals_unseen: bool,
    /// When an arrival was last said in a toast (the worker's own notice is not repeated then).
    pub arrival_said: Option<Instant>,
    /// The first payment this wallet ever received, until Activity is opened: Home's recent
    /// activity says so in its title.
    pub first_payment: Option<String>,
    /// Time locks that opened since Accounts or Qi was last looked at, said in Home's attention
    /// panel until they are ("2.4 Qi unlocked · spendable now").
    pub unlocked_news: Vec<String>,
    /// When the bell last rang (it rings at most once in ten seconds).
    pub last_bell: Option<Instant>,
}

/// Work running off the UI thread: wallet creation, endpoint checks, contract lookups, the clipboard.
pub struct Tasks {
    pub creating: Option<Creation>,
    /// A monitoring endpoint being verified (network id, endpoint, outcome).
    pub monitor_check: Option<std::sync::mpsc::Receiver<MonitorCheck>>,
    /// An IPFS gateway being tested before it is saved.
    pub ipfs_check: Option<std::sync::mpsc::Receiver<IpfsCheck>>,
    /// The destination the send form has asked about and is still waiting on.
    pub contract_probe: Option<String>,
    /// What the last answered probe found, kept while the form that asked is open.
    pub contract_found: Option<wallet_core::contracts::Discovered>,
    /// Destinations already asked about on this form, and whether an answer came back at all.
    ///
    /// Without this a probe that fails — an unresolvable address, the node down, a Qi destination
    /// — clears both fields above, so the next keystroke sees nothing remembered and asks again,
    /// and the form turns into one chain read per keypress for as long as it stays open.
    pub contract_asked: std::collections::HashSet<String>,
    /// A copy for the frame loop to carry out after the next frame (see `clipboard`).
    pub clipboard: Option<super::clipboard::CopyRequest>,
    /// A copy under way, answering with how it went.
    pub copying: Option<std::sync::mpsc::Receiver<(super::clipboard::CopyRequest, super::clipboard::Outcome)>>,
}

/// What the app is doing and has said: toasts, the busy and signing lines, the log.
pub struct Status {
    pub toasts: Vec<Toast>,
    pub busy: Option<String>,
    /// What the signing lane is doing, kept apart from `busy` so only that lane clears it.
    pub signing: Option<String>,
    /// When the current busy and signing labels began (for how long they have run).
    pub busy_at: Option<Instant>,
    pub signing_at: Option<Instant>,
    /// Recent messages (toasts disappear; this keeps them readable).
    pub log: std::collections::VecDeque<Toast>,
    /// Desktop notices the UI itself raises (a limit reachable), sent while the window is in
    /// the background like the worker's own.
    pub notices_out: Vec<(String, String)>,
    /// Kind of the review being committed (follow-ups after submission).
    pub committing_kind: Option<wallet_core::journal::OpKind>,
    /// Who a Qi send asked to be told this wallet's payment code, from the form.
    pub announce_asked: Option<String>,
    /// Who to announce to once the Qi send being committed is sent: set at approval, when its
    /// review says they have not been told.
    pub announce_after: Option<String>,
}

/// The lock screen: locked or not, the password being typed, the unlock in flight and what it said.
pub struct Lock {
    pub pending: Option<Zeroizing<String>>,
    pub locked: bool,
    pub input: String,
    /// The frame a finished lock effect left behind, dissolving under the one that replaced it.
    pub fade: Option<(String, Instant)>,
    /// The lock screen's one effect has played (or was cut short by typing); it stays still now.
    pub rested: bool,
    pub error: Option<String>,
    /// An unlock is in flight. The vault's KDF is deliberately slow, so the lock screen says so
    /// while the password is checked.
    pub unlocking: bool,
    /// When that unlock was submitted, so one that never answers is given up on.
    pub unlocking_since: Option<Instant>,
    /// A standalone unlock's password, kept until it opens the wallet to hand it to the daemon
    /// (`daemon_share_unlock`). A daemon-hosted engine shares keys itself.
    pub handoff_pending: Option<(String, Zeroizing<String>)>,
    /// A wallet switch locked the screen before the worker reached it. The worker's own `Locked`
    /// for that switch arrives later and must not lock a wallet unlocked in the meantime.
    pub switch_pending: bool,
    pub warned: bool,
    /// A review the lock discarded, said once the wallet is unlocked again.
    pub dropped_review: Option<String>,
    pub unlocked_at: Option<Instant>,
    /// A password on its way to the daemon: the answer (wallet name, or why not).
    pub handoff: Option<std::sync::mpsc::Receiver<std::result::Result<String, String>>>,
}

/// Effects: the ambient scene, the header's lights, row and drawer flashes, and the small celebrations.
pub struct Fx {
    /// Whole-main-area ambient effect (lock screen, easter egg).
    pub ambient: Option<Ceremony>,
    /// Border draw-in started on the last screen change.
    pub edge_intro: Option<Instant>,
    /// Order of the block behind the last heartbeat: 0 prime, 1 region, 2 zone.
    pub beat_order: u8,
    /// Recent head hashes (newest last), for `:poem`.
    pub recent_hashes: std::collections::VecDeque<String>,
    /// Operations that just reached the confirmation target (row flash start).
    pub row_flash: HashMap<String, Instant>,
    /// Qi denominations that just received a coin (cash drawer flash start).
    pub drawer_flash: HashMap<u8, Instant>,
    /// Haiku to decrypt when the `:poem` hash rain ends.
    pub poem_haiku: Option<String>,
    pub bell: bool,
    pub beat: Option<Instant>,
    /// A one-shot light running along the header hairline (`edge::Signal`), and when it began.
    pub hairline: Option<(super::edge::Signal, Instant)>,
    /// The hero's `▌` lights when value arrives, fading back (Full and Vivid).
    pub gutter_flash: Option<Instant>,
    /// When the focused panel's data last changed: its border glints once (`edge::paint`).
    pub glint_at: Option<Instant>,
    /// A transaction that just left the pending pill, said in its place for a moment.
    pub pill_resolved: Option<(String, Instant)>,
    /// How far into the Konami code the keys in Help have come.
    pub konami: usize,
    /// The last frame showed a spinner (a loading line, a status): it turns at its own rate.
    pub spun: bool,
    /// The smallest head hash seen this session, and its height: the entropy minimum `:poem`
    /// talks about, on the Network screen.
    pub lowest_hash: Option<(String, u64)>,
    /// Drawer slots that got a coin since Qi was last on screen (marked with `•` there).
    pub drawer_new: std::collections::HashSet<u8>,
}

/// The keyboard and pointer: hit areas and lists on screen, the pointer, holds, and when input last came.
pub struct Input {
    /// This frame's clickable regions (written while drawing, read by `pointer`).
    pub hits: std::cell::RefCell<super::hit::HitMap>,
    /// Each list's own scroll position.
    pub lists: std::cell::RefCell<HashMap<super::hit::ListId, super::hit::ListState>>,
    pub pointer: super::pointer::PointerState,
    /// The mouse was handed back to the terminal for text selection (this session only).
    pub mouse_released: bool,
    /// When the open modal appeared, and which kind it is (clicks right after are ignored).
    pub modal_since: std::cell::Cell<Option<Instant>>,
    pub modal_kind: std::cell::Cell<u8>,
    /// Hold to sign: the review being held (its op id), when Enter went down, and when the last
    /// repeat arrived.
    pub hold: Option<(String, Instant, Instant)>,
    pub last_input: Instant,
    /// The paste check has been suggested this session (after the first address copied).
    pub paste_hint_shown: bool,
    /// Where the keyboard's focus is on screen (the hidden terminal cursor waits there).
    pub focus_at: Option<(u16, u16)>,
}

/// The terminal: its capabilities, colors, pictures and links, its size and focus.
pub struct TermState {
    pub qr_rect: Option<(ratatui::layout::Rect, String)>,
    /// Text drawn larger than a cell this frame (OSC 66), placed after the cells by the loop.
    pub big_text: std::cell::RefCell<Vec<super::term::backend::BigText>>,
    /// The wallet's own hashes and addresses, for hyperlinks (`links`).
    pub links_known: std::cell::RefCell<super::links::Known>,
    /// The links the last frame showed (for tooltips and the pointer).
    pub links_shown: std::cell::RefCell<Vec<super::links::Link>>,
    pub light_hint: bool,
    pub no_color: bool,
    pub caps: Caps,
    pub kitty: KittyGraphics,
    pub pending_theme_reload: bool,
    /// Session-only theme (QUAI_TERMINAL_THEME); cleared when a theme is chosen.
    pub theme_override: Option<String>,
    /// Session-only plain mode (NO_COLOR, Linux console): no block digits, reduced motion.
    pub plain: bool,
    /// The terminal's own background (OSC 11), when it said.
    pub background: Option<(u8, u8, u8)>,
    /// The terminal size of the last frame.
    pub last_size: (u16, u16),
    /// The terminal's size class this frame (`ui::layout`).
    pub breakpoint: super::ui::Breakpoint,
    /// Too few rows for full headlines this frame: heroes shrink to one line.
    pub short: bool,
    /// The terminal window has focus (ambient light pauses without it).
    pub focused: bool,
    /// When the window last regained focus (the first click after it is ignored).
    pub focus_gained_at: Option<Instant>,
}

/// Where the user is: the screen and pane, the cursor, the detail stack, tabs and history.
pub struct Nav {
    pub screen: Screen,
    /// Focused pane within the view (Tab / Shift-Tab).
    pub pane: usize,
    pub selected: usize,
    pub jump_pending: Option<char>,
    /// The last screen switch (`start_transition`), for fast hands.
    pub last_switch: Option<Instant>,
    /// Last sub-tab per section.
    pub section_tabs: [usize; 7],
    /// Where each screen was left: its pane and cursor, and the row the cursor was on.
    pub views: HashMap<Screen, ViewState>,
    /// Screens visited, oldest first (Backspace and ctrl-o go back through them).
    pub history: Vec<Screen>,
    /// A step back through `history` is under way (it is not a new visit).
    pub going_back: bool,
    /// The exchange's card, shown or last shown: the Exchange tab returns to it.
    pub card: Card,
    /// Detail stack (Enter pushes, Esc pops).
    pub detail: Vec<Detail>,
    /// Selection inside the top detail view (collection items, listings).
    pub detail_selected: usize,
    /// Lines the help overlay is scrolled down.
    pub help_scroll: u16,
    /// The help overlay shows the one-time "what moved" note.
    pub help_moved: bool,
    /// Activity filter tab.
    pub activity_filter: ActivityFilter,
    /// Activity shows only the account that acts (`.` on Activity).
    pub activity_account_only: bool,
}

pub struct App {
    pub paths: wallet_core::paths::Paths,
    pub registry: wallet_core::registry::Registry,
    pub network_id: String,
    pub config: AppConfig,
    pub theme: Theme,
    pub meta: Option<WalletMeta>,
    /// The engine: in the daemon, or here when standalone ([`quai_engine::client::Engine`]).
    pub worker: Option<quai_engine::client::Engine>,
    pub dash: Dashboard,
    /// The layer on top: the one drawn and the one keys go to.
    pub modal: Modal,
    /// The layers under it, oldest first: closing the top one shows the one beneath (a review's
    /// form, the key overlay under the glossary). Assigning `modal` replaces the top only.
    pub beneath: Vec<Modal>,
    pub onboarding: Option<Onboarding>,
    /// A wallet switch is waiting for the new wallet's accounts before the open view can ask for
    /// anything. Set when the switch starts, cleared by the first dashboard that has accounts.
    pub reload_view_on_accounts: bool,
    pub quit: bool,
    pub dirty: bool,
    pub last_frame: Instant,
    /// A non-secret form set aside by auto-lock, restored after unlock.
    pub parked: Option<Form>,
    /// Writes preferences off the UI thread.
    pub persist: super::persist::Lane,
    /// The last frame's composed content before the edge light (see `ui::draw_edges`).
    pub content: Option<ratatui::buffer::Buffer>,
    /// The open network's profile, and what it was built for (`net`).
    pub net_cache: std::cell::RefCell<Option<(String, std::rc::Rc<wallet_core::network::NetworkProfile>)>>,
    /// `activity_rows`, and the fingerprint of what it was built from.
    pub activity_cache: std::cell::RefCell<Option<(u64, Vec<ActivityRow>)>>,
    /// Markets and the swap card are drawn side by side (the trader layout, at this width).
    pub trader: bool,
    /// Moves on at every lock and every wallet or network switch. A decrypted conversation that
    /// was asked for under an older value is dropped when it arrives, so nothing private
    /// reappears after the wallet locked or changed.
    pub private_epoch: u64,
    /// Palette entries chosen lately, newest first (`palette::Entry::key`).
    pub palette_recent: Vec<String>,
    /// Ecosystem data: portfolio, images, swaps, NFTs.
    pub eco: super::eco::Eco,
    /// Background data worker (explorer, prices, images, quotes).
    pub data: Option<super::data::DataWorker>,
    /// The chat docked beside every screen.
    pub dock: Dock,
    /// Every wallet on this computer, for the Wallets screen.
    pub cockpit: Cockpit,
    /// What arrived, and how it was said.
    pub news: News,
    /// Work running off the UI thread for this screen.
    pub tasks: Tasks,
    /// What the app is doing and has said.
    pub status: Status,
    /// The lock screen and the unlock in flight.
    pub lock: Lock,
    /// Effects: lights, flashes and the ambient scene.
    pub fx: Fx,
    /// The keyboard and pointer: what can be clicked and what was pressed.
    pub input: Input,
    /// The terminal: what it can do and how the frame fits it.
    pub term: TermState,
    /// Where the user is: screen, pane, cursor, history.
    pub nav: Nav,
}

mod actions;
pub(crate) use actions::mode_note;
mod activity;
mod events;
mod forms;
mod keys;

impl App {
    pub fn new(
        paths: wallet_core::paths::Paths,
        network_id: String,
        config: AppConfig,
        theme: Theme,
        caps: Caps,
        meta: Option<WalletMeta>,
    ) -> Self {
        let signable = meta.as_ref().is_some_and(|m| m.kind != WalletKind::Watch);
        let mut eco = super::eco::Eco::default();
        eco.swap.slippage_bps = config.swap_slippage_bps;
        eco.swap.deadline_minutes = config.swap_deadline_minutes;
        Self {
            registry: wallet_core::registry::Registry::new(paths.clone()),
            paths,
            network_id,
            config,
            theme,
            onboarding: if meta.is_none() { Some(Onboarding::Choose { selected: 0 }) } else { None },
            reload_view_on_accounts: false,
            meta,
            worker: None,
            dash: Dashboard::default(),
            modal: Modal::None,
            beneath: Vec::new(),
            quit: false,
            dirty: true,
            last_frame: Instant::now(),
            parked: None,
            activity_cache: std::cell::RefCell::new(None),
            net_cache: std::cell::RefCell::new(None),
            content: None,
            persist: super::persist::Lane::start(),
            trader: false,
            private_epoch: 0,
            palette_recent: Vec::new(),
            eco,
            data: None,
            dock: Dock { shown: false, focus: false, draft: String::new() },
            cockpit: Cockpit { list: Vec::new(), summaries: HashMap::new(), quai: HashMap::new() },
            news: News { arrivals_unseen: false, arrival_said: None, first_payment: None, unlocked_news: Vec::new(), last_bell: None },
            tasks: Tasks {
                creating: None,
                monitor_check: None,
                ipfs_check: None,
                contract_probe: None,
                contract_found: None,
                contract_asked: Default::default(),
                clipboard: None,
                copying: None,
            },
            status: Status {
                toasts: Vec::new(),
                busy: None,
                signing: None,
                busy_at: None,
                signing_at: None,
                log: std::collections::VecDeque::new(),
                notices_out: Vec::new(),
                committing_kind: None,
                announce_asked: None,
                announce_after: None,
            },
            lock: Lock {
                pending: None,
                locked: signable,
                input: String::new(),
                fade: None,
                rested: false,
                error: None,
                unlocking: false,
                unlocking_since: None,
                handoff_pending: None,
                switch_pending: false,
                warned: false,
                dropped_review: None,
                unlocked_at: None,
                handoff: None,
            },
            fx: Fx {
                ambient: None,
                edge_intro: None,
                beat_order: 2,
                recent_hashes: std::collections::VecDeque::new(),
                row_flash: HashMap::new(),
                drawer_flash: HashMap::new(),
                poem_haiku: None,
                bell: false,
                beat: None,
                hairline: None,
                gutter_flash: None,
                glint_at: None,
                pill_resolved: None,
                konami: 0,
                spun: false,
                lowest_hash: None,
                drawer_new: Default::default(),
            },
            input: Input {
                hits: Default::default(),
                lists: Default::default(),
                pointer: Default::default(),
                mouse_released: false,
                modal_since: Default::default(),
                modal_kind: Default::default(),
                hold: None,
                last_input: Instant::now(),
                paste_hint_shown: false,
                focus_at: None,
            },
            term: TermState {
                qr_rect: None,
                big_text: Default::default(),
                links_known: Default::default(),
                links_shown: Default::default(),
                light_hint: false,
                no_color: false,
                caps,
                kitty: KittyGraphics::default(),
                pending_theme_reload: false,
                theme_override: None,
                plain: false,
                background: None,
                last_size: (80, 24),
                breakpoint: Default::default(),
                short: false,
                focused: true,
                focus_gained_at: None,
            },
            nav: Nav {
                screen: Screen::Home,
                pane: 0,
                selected: 0,
                jump_pending: None,
                last_switch: None,
                section_tabs: [0; 7],
                views: HashMap::new(),
                history: Vec::new(),
                going_back: false,
                card: Card::Swap,
                detail: Vec::new(),
                detail_selected: 0,
                help_scroll: 0,
                help_moved: false,
                activity_filter: ActivityFilter::All,
                activity_account_only: false,
            },
        }
    }

    /// First-run onboarding starts in the theme showroom.
    pub fn start_onboarding(&mut self) {
        if self.meta.is_none() {
            self.onboarding = Some(Onboarding::Welcome);
        }
    }

    pub fn motion(&self) -> Motion {
        // Plain is still: a screen reader has nothing to gain from motion, and every frame it
        // would cost is one more thing re-read.
        if self.term.plain {
            Motion::Off
        } else if self.term.caps.ssh && self.config.motion.effects() {
            Motion::Reduced
        } else {
            self.config.motion
        }
    }

    /// The page color the terminal should draw as its own background, so a translucent window
    /// stays translucent. `None` paints the theme's color.
    pub fn see_through(&self) -> Option<ratatui::style::Color> {
        use wallet_core::config::BackgroundMode;
        let ratatui::style::Color::Rgb(r, g, b) = self.theme.surface else { return None };
        match self.config.background {
            BackgroundMode::Solid => None,
            BackgroundMode::Terminal => Some(self.theme.surface),
            // Only a match: the terminal's background under a different theme would change
            // every contrast the theme was checked against.
            BackgroundMode::Auto => {
                let (tr, tg, tb) = self.term.background?;
                let close = |a: u8, b: u8| a.abs_diff(b) <= 3;
                (close(r, tr) && close(g, tg) && close(b, tb)).then_some(self.theme.surface)
            }
        }
    }

    /// The open network's profile. `config.network` rebuilds every built-in profile per call
    /// (about 5 µs); drawing asks for it several times per row, so it is kept until the network
    /// or its configuration changes.
    pub fn net(&self) -> Option<std::rc::Rc<wallet_core::network::NetworkProfile>> {
        let key = format!("{}|{:?}|{:?}", self.network_id, self.config.networks, self.config.monitor_endpoints.get(&self.network_id));
        if let Some((k, n)) = self.net_cache.borrow().as_ref()
            && *k == key
        {
            return Some(n.clone());
        }
        let n = std::rc::Rc::new(self.config.network(&self.network_id).ok()?);
        *self.net_cache.borrow_mut() = Some((key, n.clone()));
        Some(n)
    }

    /// The screen pane drawn as focused: none while the pinned chat has the keyboard, so there
    /// is one lit panel. (Input reads `pane`; drawing reads this.)
    pub fn lit_pane(&self) -> Option<usize> {
        (!self.dock.focus).then_some(self.nav.pane)
    }

    /// The account picker, on the account that acts now.
    pub fn open_account_picker(&mut self) {
        let active = self.dash.active_account().map(|a| a.address.clone());
        let here = active.and_then(|a| self.dash.accounts.iter().position(|x| x.address == a)).unwrap_or(0);
        self.modal = Modal::Accounts { selected: here };
    }

    /// Make the `index`th account the one that acts. Shown at once; the worker saves it to the
    /// wallet's metadata, where the command line (`account use`) and the daemon read it too.
    pub fn use_account(&mut self, index: usize) {
        let Some(account) = self.dash.accounts.get(index).cloned() else { return };
        // Each account messages as itself: another one's conversations are not this one's.
        if self.dash.active_account().is_some_and(|a| !a.address.eq_ignore_ascii_case(&account.address)) {
            self.forget_private();
        }
        for meta in [self.dash.meta.as_mut(), self.meta.as_mut()].into_iter().flatten() {
            meta.active_account = Some(account.address.clone());
        }
        self.send(Cmd::UseAccount(account.address.clone()));
        self.toast(format!("{} acts now", account.label), false);
        // Cards quote for their owner, so they are asked again for this one.
        self.on_view_opened();
    }

    /// The wallet switcher, on the open wallet.
    pub fn open_wallet_switcher(&mut self) {
        self.load_wallets();
        let here = self.meta.as_ref().and_then(|m| self.cockpit.list.iter().position(|w| w.id == m.id)).unwrap_or(0);
        self.modal = Modal::Wallets { selected: here };
    }

    /// Go to a tab: the Exchange tab returns to the card last shown, or to the conversion card
    /// when that was the swap and trading is off.
    pub fn open_tab(&mut self, tab: Screen) {
        if tab == Screen::Exchange {
            let trading = self.config.features.on(Feature::Trading);
            self.show_card(if self.nav.card == Card::Swap && !trading { Card::Convert } else { self.nav.card });
        } else {
            self.switch(tab);
        }
    }

    /// The exchange, on one of its cards. The swap card is part of trading: without it, this
    /// says so and stays where it is.
    pub fn show_card(&mut self, card: Card) {
        if card == Card::Swap && !self.config.features.on(Feature::Trading) {
            self.info(format!("{} · System › Settings", Feature::Trading.off_note()));
            return;
        }
        self.nav.card = card;
        self.switch(Screen::Exchange);
    }

    /// Whether the exchange is showing this card.
    pub fn on_card(&self, card: Card) -> bool {
        self.nav.screen == Screen::Exchange && self.nav.card == card
    }

    /// Where the user is: the screen, the exchange's card, or the channels under Contacts.
    pub fn place(&self) -> super::keymap::Place {
        use super::keymap::Place;
        match self.nav.screen {
            Screen::Exchange => Place::Card(self.nav.card),
            Screen::Contacts if self.nav.pane == 1 => Place::Pane(Screen::Contacts, 1),
            s => Place::Screen(s),
        }
    }

    /// Go to a place.
    pub fn go(&mut self, place: super::keymap::Place) {
        use super::keymap::Place;
        match place {
            Place::Card(card) => self.show_card(card),
            Place::Screen(s) => self.open_tab(s),
            Place::Pane(s, pane) => {
                self.open_tab(s);
                if self.nav.screen == s {
                    self.nav.pane = pane;
                    self.nav.selected = 0;
                }
            }
        }
    }

    /// Whether the payment channels (Contacts' second pane) have the keys.
    pub fn on_channels(&self) -> bool {
        self.nav.screen == Screen::Contacts && self.nav.pane == 1
    }

    /// The glyphs this terminal draws.
    pub fn icon_set(&self) -> super::icons::Set {
        use super::icons::Set;
        use wallet_core::config::IconMode;
        match self.config.icons {
            IconMode::Nerd => Set::Nerd,
            IconMode::Unicode => Set::Unicode,
            IconMode::Ascii => Set::Ascii,
            IconMode::Auto if self.term.plain || std::env::var("TERM").is_ok_and(|t| t == "linux") => Set::Ascii,
            IconMode::Auto if super::icons::nerd_detected() => Set::Nerd,
            IconMode::Auto => Set::Unicode,
        }
    }

    /// The header and footer: raised on a solid page, the page itself when the terminal's
    /// background shows through (an opaque bar across a translucent window reads as a slab).
    pub fn bar_bg(&self) -> ratatui::style::Color {
        if self.see_through().is_some() { self.theme.surface } else { self.theme.raised }
    }

    /// How the pointer should be reported now: the setting, with `auto` meaning full on this
    /// computer, clicks only over SSH (every move would cross the network) and none on the Linux
    /// console, which has no mouse protocol worth using.
    pub fn pointer_mode(&self) -> super::term::Pointer {
        use super::term::Pointer;
        use wallet_core::config::MouseMode;
        if self.input.mouse_released {
            return Pointer::Off;
        }
        match self.config.mouse {
            MouseMode::Off => Pointer::Off,
            MouseMode::Click => Pointer::Clicks,
            MouseMode::Full => Pointer::Hover,
            MouseMode::Auto if std::env::var("TERM").is_ok_and(|t| t == "linux") => Pointer::Off,
            MouseMode::Auto if self.term.caps.ssh => Pointer::Clicks,
            MouseMode::Auto => Pointer::Hover,
        }
    }

    fn effects_allowed(&self) -> bool {
        self.motion().effects() && self.config.ceremonies
    }

    pub fn animating(&self) -> bool {
        self.fx.edge_intro.is_some_and(|s| s.elapsed().as_millis() < super::edge::INTRO_TOTAL_MS)
            || self.fx.row_flash.values().any(|s| s.elapsed().as_millis() < super::edge::FLASH_MS)
            || self.fx.drawer_flash.values().any(|s| s.elapsed().as_millis() < super::edge::FLASH_MS)
            || self.fx.gutter_flash.is_some_and(|s| s.elapsed().as_millis() < super::edge::FLASH_MS)
            || self.fx.ambient.is_some()
            || self.lock.fade.as_ref().is_some_and(|(_, at)| at.elapsed().as_millis() < 500)
            || matches!(self.modal, Modal::Effects(_))
    }

    /// Input arrived: the screen's auto-lock starts over, and the engine's with it.
    pub(crate) fn note_input(&mut self) {
        self.input.last_input = Instant::now();
        if let Some(w) = &self.worker {
            w.activity();
        }
    }

    /// A standalone engine over a worker already running (tests hand it a capturing one).
    #[cfg(test)]
    pub fn use_worker(&mut self, worker: super::worker::Worker) {
        let host = quai_engine::host::Host::over(worker, self.registry.clone(), "", false);
        self.worker = Some(quai_engine::client::Engine::Local(Box::new(host)));
    }

    pub fn send(&self, cmd: Cmd) {
        if let Cmd::Quote { direction, amount } = cmd {
            let key = self.eco.convert.protocol_key.get().wrapping_add(1).max(1);
            self.eco.convert.protocol_key.set(key);
            self.send_data(super::data::DataCmd::ProtocolQuote { key, direction, amount, card: self.on_card(Card::Convert) });
            return;
        }
        if matches!(cmd, Cmd::Prepare(_)) {
            wallet_core::diag::begin("ux.review");
        }
        if let Some(w) = &self.worker {
            w.send(cmd);
        }
    }

    /// Send a light along the header hairline. It plays in Full and Vivid; the other levels
    /// keep the news in its static form (a toast, a marker, a state).
    pub fn signal(&mut self, s: super::edge::Signal) {
        self.fx.hairline = Some((s, Instant::now()));
        self.dirty = true;
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toast_as(text, if error { Severity::Danger } else { Severity::Ok }, None);
    }

    /// A neutral note: something the person should know, neither a success nor a problem.
    pub fn info(&mut self, text: impl Into<String>) {
        self.toast_as(text, Severity::Info, None);
    }

    /// A toast of any severity; `id` names one the app will update or take back.
    pub fn toast_as(&mut self, text: impl Into<String>, level: Severity, id: Option<&'static str>) {
        let toast = Toast { text: text.into(), level, at: Instant::now(), id };
        self.status.log.push_front(toast.clone());
        self.status.log.truncate(50);
        self.status.toasts.push(toast);
        if self.status.toasts.len() > 4 {
            self.status.toasts.remove(0);
        }
        self.dirty = true;
    }

    /// Transactions that are sent and not yet mined, oldest first. They are what the corner
    /// pill counts down; settling and locked operations wait on the destination, not the miner.
    /// The Konami code, typed in Help (the one place with no text input and no money): a step
    /// of it, and true when the key belongs to it and Help should stay open. Done, it gives the
    /// session the Genesis theme.
    pub(crate) fn konami(&mut self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode::{Char, Down, Left, Right, Up};
        const CODE: [crossterm::event::KeyCode; 10] = [Up, Up, Down, Down, Left, Right, Left, Right, Char('b'), Char('a')];
        if CODE.get(self.fx.konami) == Some(&code) {
            self.fx.konami += 1;
        } else {
            self.fx.konami = usize::from(code == CODE[0]);
        }
        if self.fx.konami == CODE.len() {
            self.fx.konami = 0;
            self.theme = super::themes::genesis(&self.theme);
            self.info("Genesis · Quai red on true black, for this session");
            return true;
        }
        self.fx.konami > 0 && !matches!(code, Up | Down)
    }

    /// Hold to sign: count an Enter on this review. True once it has been held long enough;
    /// a pause longer than a key repeat starts the hold over.
    pub(crate) fn held_to_sign(&mut self, op: &str) -> bool {
        let now = Instant::now();
        match self.input.hold.as_mut() {
            Some((id, start, last)) if id == op && now.duration_since(*last) < HOLD_GAP => {
                *last = now;
                now.duration_since(*start) >= HOLD_TO_SIGN
            }
            _ => {
                self.input.hold = Some((op.to_string(), now, now));
                false
            }
        }
    }

    /// The line left in the shell on quit: what state the wallet was left in, in plain words.
    /// No balance, no animation; people quit to get back to their shell.
    pub fn quit_receipt(&self) -> Option<String> {
        self.meta.as_ref()?;
        let mut parts = vec!["Quai Terminal closed".to_string()];
        if self.can_sign() || self.meta.as_ref().is_some_and(|m| m.can_sign()) {
            parts.push("its keys were only in this process, which has ended".into());
        }
        if let Some(h) = self.dash.health.as_ref().map(|h| h.height).filter(|h| *h > 0) {
            parts.push(format!("last block #{}", wallet_core::amount::group_thousands(&h.to_string())));
        }
        let confirming = self.confirming_ops().len();
        if confirming > 0 {
            parts.push(format!(
                "{} still confirming (it doesn't need the wallet open)",
                wallet_core::amount::count(confirming, "transaction")
            ));
        }
        Some(parts.join(" · "))
    }

    /// The window's title: what the wallet is doing, never what it holds. A title is shown in
    /// task bars, window switchers and screen shares, so it names no balance and no wallet.
    pub fn window_title(&self) -> String {
        use super::icons::{Icon, Set};
        // Unicode marks: a window manager's title font may have no Nerd Font glyphs.
        let status = if self.meta.is_none() {
            String::new()
        } else if self.lock.locked {
            format!("{} locked", Icon::Locked.glyph(Set::Unicode))
        } else if let n @ 1.. = self.confirming_ops().len() {
            format!("{} {n} confirming", Icon::InFlight.glyph(Set::Unicode))
        } else if let n @ 1.. = self.dash.notifications.iter().filter(|n| !n.read).count() {
            format!("{} {n} unread", Icon::Bell.glyph(Set::Unicode))
        } else {
            String::new()
        };
        if status.is_empty() { "Quai Terminal".into() } else { format!("Quai Terminal · {status}") }
    }

    /// Taskbar progress (OSC 9;4 state): busy (3) while transactions confirm or one is being
    /// prepared, nothing (0) otherwise.
    pub fn taskbar_state(&self) -> u8 {
        if !self.lock.locked && (self.busy_label().is_some() || !self.confirming_ops().is_empty()) { 3 } else { 0 }
    }

    pub fn confirming_ops(&self) -> Vec<&wallet_core::appdb::Operation> {
        use wallet_core::appdb::OpStatus;
        let mut open: Vec<&wallet_core::appdb::Operation> =
            self.dash.ops.iter().filter(|o| matches!(o.status, OpStatus::Submitted | OpStatus::Unknown)).collect();
        open.sort_by_key(|o| o.updated);
        open
    }

    pub fn start_transition(&mut self) {
        // Borders draw themselves in (see `edge`); content appears at once, so amounts never
        // animate. Fast hands win: a switch right after another (a held `]`, digits in a row)
        // just shows the screen, with no borders blanking on every step.
        let rapid = self.nav.last_switch.is_some_and(|at| at.elapsed() < FAST_HANDS);
        self.nav.last_switch = Some(Instant::now());
        if self.motion().effects() && !rapid {
            self.fx.edge_intro = Some(Instant::now());
        } else {
            self.fx.edge_intro = None;
        }
    }

    /// The status-bar spinner's label. A transaction being prepared or sent outranks a sync, and
    /// stays up until its own lane is done, whatever the worker reports in between.
    /// The busy label as the header shows it: after a few seconds, how long it has run, and for
    /// the slow first Qi scan what it usually takes. Setting the expectation is the kindness.
    pub fn busy_text(&self) -> Option<String> {
        let (label, at) = match &self.status.signing {
            Some(s) => (s.as_str(), self.status.signing_at),
            None => (self.status.busy.as_deref()?, self.status.busy_at),
        };
        let secs = at.map(|a| a.elapsed().as_secs()).unwrap_or(0);
        let mut text = label.to_string();
        if secs >= 3 {
            text.push_str(&format!(" {secs}s"));
            if label.contains("scans Qi") {
                text.push_str(" · usually 10–15 s");
            }
        }
        Some(text)
    }

    pub fn busy_label(&self) -> Option<&str> {
        self.status.signing.as_deref().or(self.status.busy.as_deref())
    }

    pub fn can_sign(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.kind != WalletKind::Watch)
    }

    /// Seconds until auto-lock, when it applies.
    pub fn autolock_remaining(&self) -> Option<u64> {
        (!self.lock.locked && self.can_sign() && self.config.auto_lock_minutes > 0)
            .then(|| (u64::from(self.config.auto_lock_minutes) * 60).saturating_sub(self.input.last_input.elapsed().as_secs()))
    }

    /// The sections in the sidebar: those with at least one view whose feature is on.
    pub fn sections(&self) -> Vec<Section> {
        Section::ALL.into_iter().filter(|s| !s.screens(&self.shown()).is_empty()).collect()
    }

    /// What the terminal shows now.
    pub fn shown(&self) -> Shown {
        Shown { features: self.config.features, pro: self.config.mode == Mode::Pro }
    }

    /// Say why a Pro screen stays shut in a Simple terminal.
    fn say_pro(&mut self, what: &str) {
        self.info(format!("{what} is part of Pro · :pro, or System › Settings › Mode"));
    }

    /// Switch to a section, restoring its last sub-tab (or its first, if that one is turned off).
    pub fn switch_section(&mut self, section: Section) {
        let shown = self.shown();
        let screens = section.screens(&shown);
        let Some(&first) = screens.first() else {
            // Shut by Simple when a switch leaves something on in it, else by the switch.
            let switched_on = section.all_screens().iter().any(|s| s.feature().is_none_or(|f| shown.features.on(f)));
            if switched_on {
                self.say_pro(section.title());
            } else if let Some(feature) = section.all_screens().iter().find_map(|s| s.feature()) {
                self.info(format!("{} · System › Settings", feature.off_note()));
            }
            return;
        };
        // `5` is the inbox (U3 in the architecture review).
        if section == Section::People && screens.contains(&Screen::Board) {
            self.open_inbox();
            return;
        }
        let idx = Section::ALL.iter().position(|s| *s == section).unwrap_or(0);
        let last = section.all_screens().get(self.nav.section_tabs[idx]).copied();
        let target = last.filter(|s| screens.contains(s) && s.enabled(&shown)).unwrap_or(first);
        self.open_tab(target);
    }

    /// The inbox: the board, on the newest private conversation (or on private messages' own row
    /// until there is one). Messaging off, there is no inbox and this is Contacts.
    pub fn open_inbox(&mut self) {
        if !Screen::Board.enabled(&self.shown()) {
            self.open_tab(Screen::Contacts);
            return;
        }
        self.switch(Screen::Board);
        if self.nav.screen != Screen::Board {
            return;
        }
        use super::eco::BoardRow;
        let rows = self.board_rows();
        let at = rows
            .iter()
            .position(|r| matches!(r, BoardRow::Chat(..) | BoardRow::Request(_)))
            .or_else(|| rows.iter().position(|r| matches!(r, BoardRow::Messaging)));
        if let Some(i) = at {
            self.nav.pane = 0;
            self.nav.selected = i;
        }
    }

    /// Breadcrumb for the header: `Home › Portfolio › WQI`.
    pub fn breadcrumb(&self) -> Vec<String> {
        let section = self.nav.screen.section();
        let mut parts = vec![section.title().to_string()];
        if section == Section::Activity {
            parts.push(self.nav.activity_filter.title().to_string());
        } else if section.screens(&self.shown()).len() > 1 {
            parts.push(self.nav.screen.tab_title().to_string());
        }
        for d in &self.nav.detail {
            parts.push(self.detail_title(d));
        }
        parts
    }

    pub fn switch(&mut self, screen: Screen) {
        // Leaving Qi: its new-coin marks have been seen.
        if self.nav.screen == Screen::Qi && screen != Screen::Qi {
            self.fx.drawer_new.clear();
        }
        if screen == Screen::Activity {
            self.news.arrivals_unseen = false;
            self.news.first_payment = None;
        }
        if matches!(screen, Screen::Accounts | Screen::Qi) {
            self.news.unlocked_news.clear();
        }
        if let Some(feature) = screen.feature().filter(|f| !self.config.features.on(*f)) {
            self.info(format!("{} · System › Settings", feature.off_note()));
            return;
        }
        if screen.pro_only() && self.config.mode != Mode::Pro {
            self.say_pro(screen.title());
            return;
        }
        // Leaving Markets from its pair list: the chart keeps that pair wherever it is drawn next.
        if self.nav.screen == Screen::Markets && self.nav.pane == 0 && screen != Screen::Markets {
            self.eco.markets_view.pair_selected = self.nav.selected;
        }
        let section = screen.section();
        if let (Some(si), Some(ti)) =
            (Section::ALL.iter().position(|s| *s == section), section.all_screens().iter().position(|s| *s == screen))
        {
            self.nav.section_tabs[si] = ti;
        }
        if self.nav.screen != screen || !self.nav.detail.is_empty() {
            if self.nav.screen != screen {
                // Leaving: remember where, and that it was here (for going back).
                let key = self.row_key(super::hit::ListId::Screen(self.nav.screen, self.nav.pane), self.nav.selected);
                self.nav.views.insert(self.nav.screen, ViewState { pane: self.nav.pane, selected: self.nav.selected, key });
                if !self.nav.going_back {
                    self.nav.history.retain(|s| *s != self.nav.screen);
                    self.nav.history.push(self.nav.screen);
                    if self.nav.history.len() > 20 {
                        self.nav.history.remove(0);
                    }
                }
            }
            self.term.kitty.clear(self.term.caps.tmux);
            self.nav.screen = screen;
            self.nav.selected = 0;
            self.nav.pane = 0;
            self.nav.detail.clear();
            self.nav.detail_selected = 0;
            // Arriving: where this screen was left, on the same row if it is still there.
            if let Some(v) = self.nav.views.get(&screen).cloned() {
                self.nav.pane = v.pane.min(screen.panes().saturating_sub(1));
                let list = super::hit::ListId::Screen(screen, self.nav.pane);
                let len = self.list_len();
                self.nav.selected = v
                    .key
                    .as_ref()
                    .and_then(|k| (0..len).find(|i| self.row_key(list, *i).as_ref() == Some(k)))
                    .unwrap_or(if len == 0 { v.selected } else { v.selected.min(len - 1) });
            }
            if screen == Screen::Network {
                self.nav.selected = self.dash.networks.iter().position(|(id, _)| *id == self.dash.network_id).unwrap_or(0);
            }
            self.start_transition();
            self.unfocus_cards();
            self.on_view_opened();
        }
    }

    /// Backspace, ctrl-o: close an open detail, or return to the screen before this one, as it
    /// was left.
    pub fn go_back(&mut self) {
        if !self.nav.detail.is_empty() {
            self.nav.detail.pop();
            self.nav.detail_selected = 0;
            return;
        }
        while let Some(prev) = self.nav.history.pop() {
            if prev != self.nav.screen && prev.enabled(&self.shown()) {
                self.nav.going_back = true;
                self.switch(prev);
                self.nav.going_back = false;
                return;
            }
        }
        self.info("nothing before this");
    }

    /// Housekeeping every ~500ms: auto-lock, toast expiry, idle lock-screen effect.
    pub fn tick(&mut self, size: (u16, u16)) {
        if let Some(left) = self.autolock_remaining()
            && left <= 60
            && left > 0
            && !self.lock.warned
        {
            self.lock.warned = true;
            self.fx.bell = self.config.sound;
            self.toast_as(format!("locking in {left}s · any key keeps it open"), Severity::Attention, Some("autolock"));
        }
        // The countdown counts, and goes the moment a key has kept the wallet open.
        match self.autolock_remaining() {
            Some(left) if left <= 60 && self.lock.warned => {
                if let Some(t) = self.status.toasts.iter_mut().find(|t| t.id == Some("autolock")) {
                    let text = format!("locking in {left}s · any key keeps it open");
                    if t.text != text {
                        t.text = text;
                        self.dirty = true;
                    }
                }
            }
            _ => {
                let before = self.status.toasts.len();
                self.status.toasts.retain(|t| t.id != Some("autolock"));
                self.dirty |= before != self.status.toasts.len();
            }
        }
        if self.autolock_remaining() == Some(0) {
            self.lock_now(Some(size));
            self.input.last_input = Instant::now();
        }
        // A hold that stopped short lets go: the bar goes back to showing what was read.
        if self.input.hold.as_ref().is_some_and(|(_, _, last)| last.elapsed() >= HOLD_GAP) {
            self.input.hold = None;
            self.dirty = true;
        }
        if self.fx.pill_resolved.as_ref().is_some_and(|(_, at)| at.elapsed() >= PILL_RESOLVED) {
            self.fx.pill_resolved = None;
            self.dirty = true;
        }
        let before = self.status.toasts.len();
        self.status.toasts.retain(|t| t.at.elapsed().as_secs() < t.lasts());
        if before != self.status.toasts.len() {
            self.dirty = true;
        }
        // A lock screen that isn't being drawn yet (no size) gets its first effect here. Effects
        // follow one another in the draw (`draw_lock`); this picks the loop up again after typing
        // stilled it and the field was cleared. Without looping the screen rests until the next lock.
        // The fade is judged by its age, not by whether a frame has cleared it: once it is over,
        // nothing animates, so no frame is drawn to clear it.
        let faded = self.lock.fade.as_ref().is_none_or(|(_, at)| at.elapsed().as_millis() >= super::ui::HANDOVER_MS);
        let replay = self.config.lock_loop && self.lock.rested && faded && self.lock.input.is_empty() && !self.lock.unlocking;
        if self.lock.locked && self.fx.ambient.is_none() && (!self.lock.rested || replay) && self.meta.is_some() {
            self.lock.rested = false;
            self.lock.fade = None;
            self.start_lock_ceremony(size);
        }
        if matches!(self.modal, Modal::Effects(_)) {
            self.dirty = true;
        }
        // An unlock that never answers must not leave the screen ignoring the keyboard.
        if let Some(at) = self.lock.unlocking_since
            && at.elapsed() > std::time::Duration::from_secs(30)
        {
            self.lock.unlocking = false;
            self.lock.unlocking_since = None;
            self.lock.error = Some("the wallet did not answer — try again".into());
            self.dirty = true;
        }
    }
}

/// Client-side checks before anything reaches the worker.
fn validate(form: &Form) -> Result<(), (usize, String)> {
    if matches!(form.kind, FormKind::BoundedSwap { .. }) {
        let minimum = form.fields.get(2).map(|f| f.value.trim()).unwrap_or_default();
        let impact = form.fields.get(3).map(|f| f.value.trim()).unwrap_or_default();
        if minimum.is_empty() && impact.is_empty() {
            return Err((2, "set a minimum receive or maximum impact".into()));
        }
        if !impact.is_empty() && impact.parse::<u16>().map_or(true, |v| v > 10_000) {
            return Err((3, "maximum impact must be whole basis points from 0 through 10000".into()));
        }
    }
    if matches!(form.kind, FormKind::Contact(_)) {
        return validate_contact(form);
    }
    for (i, f) in form.fields.iter().enumerate() {
        let value = f.value.trim();
        if value.is_empty() && !f.optional {
            return Err((i, format!("{} is required", f.label.to_lowercase())));
        }
        if value.is_empty() {
            continue;
        }
        let numeric = |s: &str| {
            let mut dots = 0;
            !s.is_empty()
                && s.chars().all(|c| {
                    c.is_ascii_digit()
                        || (c == '.' && {
                            dots += 1;
                            dots == 1
                        })
                })
        };
        match &f.kind {
            FieldKind::Amount(_) => {
                if !numeric(value) {
                    return Err((i, "amounts are plain decimals like 12.5".into()));
                }
            }
            _ if form.kind == FormKind::Approve && f.label == "Amount" => {
                if !value.eq_ignore_ascii_case("unlimited") && !numeric(value) {
                    return Err((i, "enter a number, 0 to revoke, or `unlimited`".into()));
                }
            }
            _ if matches!(f.label.as_str(), "Slippage") => {
                if value.parse::<u16>().map_or(true, |b| b > 10_000) {
                    return Err((i, "slippage is basis points between 0 and 10000".into()));
                }
            }
            _ if matches!(f.label.as_str(), "Runs" | "Every (minutes)") => {
                if value.parse::<u64>().map_or(true, |n| n == 0) {
                    return Err((i, format!("{} must be a whole number above 0", f.label.to_lowercase())));
                }
            }
            FieldKind::NewSecret if value.chars().count() < wallet_vault::MIN_PASSWORD_CHARS => {
                return Err((i, format!("use at least {} characters", wallet_vault::MIN_PASSWORD_CHARS)));
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_contact(form: &Form) -> Result<(), (usize, String)> {
    let get = |i: usize| form.fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
    if get(0).is_empty() {
        return Err((0, "name is required".into()));
    }
    if get(1).is_empty() && get(2).is_empty() {
        return Err((1, "add a Quai/Qi address, a payment code, or both".into()));
    }
    if !get(1).is_empty() && wallet_core::registry::parse_any_address(&get(1)).is_err() {
        return Err((1, "that isn't a valid Quai or Qi address".into()));
    }
    if !get(2).is_empty() && wallet_core::sdk::payments::PaymentCode::from_base58(&get(2)).is_err() {
        return Err((2, "that isn't a valid payment code".into()));
    }
    Ok(())
}

/// Point an error at the field it concerns.
fn error_field(form: &Form, text: &str) -> Option<usize> {
    let t = text.to_lowercase();
    let find = |labels: &[&str]| form.fields.iter().position(|f| labels.contains(&f.label.as_str()));
    if t.contains("password") {
        find(&["Password", "Backup password"])
    } else if t.contains("insufficient") || t.contains("amount") || t.contains("minimum") || t.contains("balance") {
        find(&["Amount", "Amount per run"])
    } else if t.contains("address") || t.contains("recipient") || t.contains("contact") || t.contains("payment code") {
        find(&["To", "Peer", "Spender", "Address", "Payment code", "Contract address"])
    } else if t.contains("token") {
        find(&["Token", "Contract address"])
    } else if t.contains("slippage") {
        find(&["Slippage"])
    } else if t.contains("fee") {
        find(&["Max fee"])
    } else {
        None
    }
}

/// Plain-language versions of common node and SDK errors.
pub fn friendly_error(msg: &str) -> String {
    let m = msg.to_lowercase();
    if m.contains("incorrect password") {
        // The vault cannot tell a wrong password from a damaged file, and says so — but the
        // first of those is what is actually happening, so it leads.
        "That password didn't open this wallet. Caps Lock? (A damaged vault file looks the same.)".into()
    } else if m.contains("-32000") && !m.contains("insufficient") {
        format!("the node rejected the transaction — usually the balance can't cover amount + fee ({msg})")
    } else if m.contains("insufficient funds") || m.contains("insufficient spendable") {
        "insufficient balance for this amount plus the fee".into()
    } else if m.contains("explicit fee policy") {
        format!("{msg} — the fee is above the maximum you set for this transaction, or gas is extraordinarily expensive right now")
    } else if m.contains("connection refused") || m.contains("error sending request") {
        format!("can't reach the node ({msg})")
    } else {
        msg.to_string()
    }
}

/// The shell command that prepares the same send as a review, for scripts and runbooks.
pub fn review_cli(r: &wallet_core::tx::Review) -> Option<String> {
    let amount = r.amount.split_whitespace().next()?.replace(',', "");
    let from = r.from.split_whitespace().next().filter(|f| f.starts_with("0x"));
    let from = from.map(|f| format!(" --from {f}")).unwrap_or_default();
    match r.kind.as_str() {
        "send_quai" => Some(format!("quai-terminal send quai --to {} --amount {amount}{from}", r.to)),
        "send_qi" => Some(format!("quai-terminal send qi --to {} --amount {amount}", r.to)),
        "send_token" => {
            let contract = r.fields.iter().find(|f| f.label == "Token contract")?.value.clone();
            let call = &r.fields.iter().find(|f| f.label == "Call")?.value;
            let recipient = call.strip_prefix("transfer(")?.split(',').next()?.trim().to_string();
            Some(format!("quai-terminal send token {contract} --to {recipient} --amount {amount}{from}"))
        }
        _ => None,
    }
}

/// The optional feature a palette action belongs to.
pub fn action_feature(id: &str) -> Option<Feature> {
    match id {
        "trade" | "swap" | "launches" | "pnl" => Some(Feature::Trading),
        "nfts" | "explore" | "listings" => Some(Feature::Nfts),
        _ => None,
    }
}

/// Settings rows: (id, label).
pub const SETTINGS: &[(&str, &str)] = &[
    // Appearance
    ("theme", "Theme"),
    ("motion", "Motion"),
    ("background", "Background"),
    ("icons", "Icons"),
    ("layout", "Layout"),
    ("mouse", "Mouse"),
    ("vim_keys", "Move with h j k l"),
    ("big_numbers", "Big balance digits"),
    ("balance_in_bar", "Balance in the top bar"),
    ("ceremonies", "Effects & celebrations"),
    ("lock_effect", "Lock screen animation"),
    ("lock_loop", "Loop the lock screen animation"),
    ("sound", "Terminal bell on good news"),
    // Features
    ("mode", "Mode"),
    ("feature:messaging", "Messaging"),
    ("feature:trading", "Trading"),
    ("feature:nfts", "NFTs"),
    ("notifications", "Notifications"),
    // Security
    ("autolock", "Auto-lock"),
    ("hold_to_sign", "Hold enter to sign"),
    ("phrase", "Reveal recovery phrase…"),
    ("backup", "Encrypted backup…"),
    // Privacy & data
    ("images", "NFT images and token icons"),
    ("ipfs", "IPFS gateway · pictures"),
    ("abi_ipfs", "IPFS gateway · contracts"),
    ("refresh", "Full refresh"),
    // Daemon
    ("daemon", "Background daemon"),
    ("daemon_unlock", "Unlock the daemon too"),
];

/// The group a setting sits under on the Settings screen.
pub fn setting_group(id: &str) -> &'static str {
    match id {
        "mode" | "feature:messaging" | "feature:trading" | "feature:nfts" | "notifications" => "Features",
        "autolock" | "hold_to_sign" | "phrase" | "backup" => "Security",
        "images" | "ipfs" | "abi_ipfs" | "refresh" => "Privacy & data",
        "daemon" | "daemon_unlock" => "Daemon",
        _ => "Appearance",
    }
}

/// Data sources switches (System › Data sources).
pub const DATA_SOURCES: &[(&str, &str)] = &[
    ("explorer_lookups", "Explorer lookups (holdings, NFTs, history)"),
    ("market_data", "Market data (prices, collections, listings)"),
    ("images", "NFT images"),
    ("token_icons", "Token icons"),
    ("test", "Test connection"),
];

/// Base-36 jump label for a visible row.
pub fn jump_label(i: usize) -> char {
    if i >= 36 { ' ' } else { std::char::from_digit(i as u32, 36).unwrap_or(' ') }
}

pub fn label_index(c: char) -> Option<usize> {
    c.to_digit(36).map(|d| d as usize)
}

fn default_backup_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{home}/quai-terminal-{}.qwbackup", wallet_core::registry::now())
}

/// `~`-relative path, shortened in the middle to `max` characters.
pub fn short_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let p = if !home.is_empty() && path.starts_with(&home) { format!("~{}", &path[home.len()..]) } else { path.to_string() };
    let chars: Vec<char> = p.chars().collect();
    if chars.len() <= 40 {
        return p;
    }
    let head: String = chars[..14].iter().collect();
    let tail: String = chars[chars.len() - 24..].iter().collect();
    format!("{head}…{tail}")
}

/// Rough password strength 0–4 (length and character classes).
pub fn password_strength(p: &str) -> u8 {
    let len = p.chars().count();
    let classes = [
        p.chars().any(|c| c.is_lowercase()),
        p.chars().any(|c| c.is_uppercase()),
        p.chars().any(|c| c.is_ascii_digit()),
        p.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    match (len, classes) {
        (0..=7, _) => 0,
        (8..=11, 0..=2) => 1,
        (8..=11, _) | (12..=15, 0..=1) => 2,
        (12..=15, _) | (16..=19, _) => 3,
        _ => 4,
    }
}

#[cfg(test)]
#[path = "../app_tests.rs"]
mod tests;

/// What to type for one ABI type, in the words someone filling in a form needs.
pub fn argument_hint(ty: &str) -> String {
    if ty.ends_with(']') || ty.starts_with('(') {
        return format!("{ty} · a JSON list, e.g. [\"0x00…\", \"1\"]");
    }
    match ty {
        "address" => "a Quai address (0x00…)".into(),
        "bool" => "true or false".into(),
        "string" => "text".into(),
        "bytes" => "hex, e.g. 0x00ff".into(),
        t if t.starts_with("bytes") => format!("{t} · hex, e.g. 0x00ff"),
        t if t.starts_with("uint") || t.starts_with("int") => format!("{t} · a whole number, in the smallest unit"),
        other => other.into(),
    }
}

#[cfg(test)]
pub(crate) fn modal_name(m: &Modal) -> &'static str {
    match m {
        Modal::None => "none",
        Modal::Form(_) => "form",
        Modal::Review(_) => "review",
        Modal::Help => "help",
        Modal::Glossary { .. } => "glossary",
        Modal::Palette { .. } => "palette",
        Modal::Receive { .. } => "receive",
        Modal::Secret { .. } => "secret",
        Modal::Quote(_) => "quote",
        Modal::Result(_) => "result",
        Modal::Notifications => "notifications",
        Modal::Notice { .. } => "notice",
        Modal::Confirm { .. } => "confirm",
        Modal::Themes(_) => "themes",
        Modal::Effects(_) => "effects",
        Modal::TokenPicker { .. } => "token picker",
        Modal::Sheet { .. } => "sheet",
        Modal::Wallets { .. } => "wallets",
        Modal::Accounts { .. } => "accounts",
        Modal::GoTo => "go to",
    }
}
