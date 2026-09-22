//! TUI application state and input handling.

use super::fx::Ceremony;
use super::terminal::{Caps, KittyGraphics};
use super::theme::Theme;
use super::worker::{Cmd, Dashboard, Ev, Prepare, Worker};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use std::collections::HashMap;
use std::time::Instant;
use wallet_core::appdb::OpStatus;
use wallet_core::config::{AppConfig, Feature, Features, Motion};
use wallet_core::ops::ConversionQuote;
use wallet_core::registry::{WalletKind, WalletMeta};
use wallet_core::tx::{Review, Submitted};
use zeroize::{Zeroize, Zeroizing};

/// Top-level sections (number keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Home,
    Trade,
    Nfts,
    People,
    Activity,
    System,
}

impl Section {
    pub const ALL: [Section; 6] = [Section::Home, Section::Trade, Section::Nfts, Section::People, Section::Activity, Section::System];

    pub fn title(self) -> &'static str {
        match self {
            Section::Home => "Home",
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
            Section::Trade => '2',
            Section::Nfts => '3',
            Section::People => '4',
            Section::Activity => '5',
            Section::System => '0',
        }
    }

    /// Every view behind the sub-tabs, whether its feature is on or not (Activity has one view
    /// with filter tabs).
    pub fn all_screens(self) -> &'static [Screen] {
        match self {
            Section::Home => &[Screen::Home, Screen::Qi, Screen::Accounts, Screen::Locks],
            Section::Trade => &[Screen::Markets, Screen::Swap, Screen::Pools, Screen::Convert, Screen::Wrap, Screen::Launches],
            Section::Nfts => &[Screen::Collected, Screen::Explore, Screen::Listings],
            Section::People => &[Screen::Contacts, Screen::Channels, Screen::Board],
            Section::Activity => &[Screen::Activity],
            Section::System => &[Screen::Wallets, Screen::Network, Screen::Settings, Screen::DataSources],
        }
    }

    /// The views shown with these features on: a section whose views are all turned off is
    /// hidden altogether.
    pub fn screens(self, features: &Features) -> Vec<Screen> {
        self.all_screens().iter().copied().filter(|s| s.enabled(features)).collect()
    }

    /// Sub-tab labels.
    pub fn tab_labels(self, features: &Features) -> Vec<&'static str> {
        match self {
            Section::Activity => ActivityFilter::ALL.iter().map(|f| f.title()).collect(),
            _ => self.screens(features).iter().map(|s| s.title()).collect(),
        }
    }
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    Qi,
    Accounts,
    Locks,
    Markets,
    Swap,
    /// Liquidity positions, adding, removing and gauge staking.
    Pools,
    Convert,
    Wrap,
    /// Quainance's launch zone: bonding-curve launches and where they trade now.
    Launches,
    Collected,
    Explore,
    Listings,
    Contacts,
    Channels,
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
    #[cfg(test)]
    pub const ALL: [Screen; 21] = [
        Screen::Home,
        Screen::Qi,
        Screen::Accounts,
        Screen::Locks,
        Screen::Markets,
        Screen::Swap,
        Screen::Pools,
        Screen::Convert,
        Screen::Wrap,
        Screen::Launches,
        Screen::Collected,
        Screen::Explore,
        Screen::Listings,
        Screen::Contacts,
        Screen::Channels,
        Screen::Board,
        Screen::Wallets,
        Screen::Activity,
        Screen::Network,
        Screen::Settings,
        Screen::DataSources,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Screen::Home => "Portfolio",
            Screen::Qi => "Qi coins",
            Screen::Accounts => "Accounts",
            Screen::Locks => "Locks",
            Screen::Markets => "Markets",
            Screen::Swap => "Swap",
            Screen::Pools => "Pools",
            Screen::Convert => "Convert",
            Screen::Wrap => "Wrap",
            Screen::Launches => "Launches",
            Screen::Collected => "Collected",
            Screen::Explore => "Explore",
            Screen::Listings => "Listings",
            Screen::Contacts => "Contacts",
            Screen::Channels => "Channels",
            Screen::Board => "Board",
            Screen::Wallets => "Wallets",
            Screen::Activity => "Activity",
            Screen::Network => "Network",
            Screen::Settings => "Settings",
            Screen::DataSources => "Data sources",
        }
    }

    pub fn section(self) -> Section {
        Section::ALL.iter().copied().find(|s| s.all_screens().contains(&self)).unwrap_or(Section::Home)
    }

    /// The optional feature this view belongs to. Convert and Wrap sit under Trade but are wallet
    /// operations, so they stay when trading is off.
    pub fn feature(self) -> Option<Feature> {
        match self {
            Screen::Markets | Screen::Swap | Screen::Pools | Screen::Launches => Some(Feature::Trading),
            Screen::Collected | Screen::Explore | Screen::Listings => Some(Feature::Nfts),
            Screen::Board => Some(Feature::Messaging),
            _ => None,
        }
    }

    pub fn enabled(self, features: &Features) -> bool {
        self.feature().is_none_or(|f| features.on(f))
    }

    /// Panes that Tab / Shift-Tab move focus between.
    pub fn panes(self) -> usize {
        match self {
            Screen::Home | Screen::Markets | Screen::Board | Screen::Pools => 2,
            Screen::Swap | Screen::Convert | Screen::Wrap => 1,
            Screen::Explore => 1,
            _ => 1,
        }
    }
}

/// Hints the footer shows beside `:` palette and `?`: what the focused thing can do right now,
/// most useful first, never more than [`FOOTER_HINTS`]. Everything else is under `?`.
pub fn context_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    if app.dock_focus {
        return vec![("enter", "post · review"), ("tab", "back to the screen"), ("esc", "leave, keep draft"), ("ctrl-u", "clear")];
    }
    let mut out: Vec<(&'static str, &'static str)> = match app.screen {
        Screen::Swap => match app.eco.swap.field {
            5 => vec![("tab", "edit"), ("/", "pick token"), ("%", "25·50·75%"), ("f", "flip")],
            1 if app.swap_quote_current() && app.eco.swap.quote.as_ref().is_some_and(|q| q.is_ok()) => {
                vec![("enter", "review"), ("m", "max"), ("%", "share"), ("esc", "done")]
            }
            1 => vec![("0-9", "amount"), ("m", "max"), ("%", "25·50·75%"), ("esc", "done")],
            0 | 2 => vec![("enter", "pick token"), ("f", "flip"), ("tab", "next"), ("esc", "done")],
            _ => vec![("←→", "adjust"), ("tab", "next"), ("esc", "done")],
        },
        Screen::Markets if app.pane == 1 => vec![("j/k", "swap"), ("o", "open tx"), ("m", "hide dust"), ("tab", "pairs")],
        Screen::Markets => vec![("t", "trade pair"), ("w", "watch"), ("A", "alert"), ("tab", "flow")],
        Screen::Home if app.pane == 1 => vec![("enter", "detail"), ("y", "copy tx"), ("tab", "holdings")],
        Screen::Home => vec![("enter", "detail"), ("t", "swap"), ("s", "send"), ("r", "receive")],
        screen => screen_hints(screen).to_vec(),
    };
    if !app.config.features.trading {
        out.retain(|(_, what)| !matches!(*what, "swap" | "trade"));
    }
    out.truncate(FOOTER_HINTS);
    out
}

/// Screen hints in the footer at once; with `:` and `?` that is six keys to read, not twelve.
pub const FOOTER_HINTS: usize = 4;

/// Key hints per view: the single source for the footer and the help overlay.
pub fn screen_hints(screen: Screen) -> &'static [(&'static str, &'static str)] {
    match screen {
        Screen::Home => &[
            ("tab", "holdings·activity"),
            ("enter", "detail"),
            ("t", "swap"),
            ("s", "send"),
            ("r", "receive"),
            ("i", "import token"),
            ("D", "discover"),
            ("y", "copy"),
        ],
        Screen::Pools => {
            &[("j/k", "position"), ("a", "add"), ("r", "remove"), ("s", "stake"), ("u", "unstake"), ("h", "harvest"), ("R", "refresh")]
        }
        Screen::Accounts => &[("a", "add account"), ("e", "rename"), ("y", "copy"), ("s", "send"), ("r", "receive")],
        Screen::Activity => {
            &[("[ ]", "filter"), ("enter", "detail"), ("'", "jump"), ("y", "copy tx"), ("u", "speed up"), ("o", "explorer link")]
        }
        Screen::Qi => {
            &[("s", "send"), ("r", "receive"), ("a", "new address"), ("S", "scan"), ("A", "aggregate"), ("W", "sweep"), ("y", "copy")]
        }
        Screen::Contacts => {
            &[("enter", "pay"), ("a", "add"), ("e", "edit"), ("n", "notify"), ("Q", "send QUAI"), ("y", "copy"), ("x", "remove")]
        }
        Screen::Channels => {
            &[("enter", "pay Qi"), ("a", "save as contact"), ("S", "rescan"), ("n", "notify"), ("d", "scan mailbox"), ("y", "copy")]
        }
        Screen::Wallets => &[("enter", "open wallet"), ("a", "new wallet"), ("i", "import phrase"), ("e", "rename"), ("y", "copy address")],
        Screen::Board => &[
            ("tab", "list·messages"),
            ("p", "post · write"),
            ("a", "new channel"),
            ("/", "filter"),
            ("c", "sender to contacts"),
            ("P", "pin beside every screen"),
            ("n", "notify me"),
            ("x", "remove"),
        ],
        Screen::Markets => &[
            ("tab", "pairs·flow"),
            ("L", "sort by TVL"),
            ("M", "sort by 24h"),
            ("T", "timeframe"),
            ("f", "flip pair"),
            ("w", "watch pair"),
            ("A", "alert on pair"),
            ("m", "hide dust"),
            ("t", "trade pair"),
            ("o", "link"),
        ],
        Screen::Swap => &[
            ("0-9", "amount"),
            ("m", "max"),
            ("%", "25·50·75%"),
            ("tab", "field"),
            ("/", "pick token"),
            ("E", "exact output"),
            ("B", "swap bounds"),
            ("O", "create order"),
            ("L", "orders"),
            ("f", "flip"),
            ("←→", "adjust"),
            ("enter", "review"),
        ],
        Screen::Convert => {
            &[("0-9", "amount"), ("m", "max"), ("tab", "field"), ("←→", "direction"), ("f", "flip"), ("enter", "quote · review")]
        }
        Screen::Wrap => &[("0-9", "amount"), ("m", "max"), ("tab", "field"), ("←→", "mode"), ("enter", "review")],
        Screen::Locks => &[("j/k", "move"), ("R", "refresh")],
        Screen::Launches => &[
            ("j/k", "move"),
            ("b", "buy"),
            ("S", "sell"),
            ("c", "claim"),
            ("t", "swap (pooled)"),
            ("y", "copy"),
            ("o", "link"),
            ("R", "reload"),
        ],
        Screen::Collected => {
            &[("hjkl", "move"), ("enter", "detail"), ("L", "list for sale"), ("X", "cancel listing"), ("T", "transfer"), ("R", "reload")]
        }
        Screen::Explore => &[("/", "search"), ("enter", "collection"), ("S", "sort"), ("R", "reload")],
        Screen::Listings => &[
            ("enter", "detail"),
            ("b", "buy"),
            ("m", "yours"),
            ("S", "sort"),
            ("f/F", "collection"),
            ("o", "Bazarr link"),
            ("R", "reload"),
        ],
        Screen::Network => &[("enter", "switch network"), ("m", "monitoring endpoint"), ("R", "refresh")],
        Screen::Settings => &[("enter", "change"), ("T", "theme showroom"), ("L", "lock screen gallery")],
        Screen::DataSources => &[("enter", "toggle"), ("t", "test connection")],
    }
}

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
    /// Send a sealed message to one peer.
    BoardDm {
        peer: String,
        name: Option<String>,
    },
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
    RenameAccount(String),
    NewQiAddress,
    /// Add (None) or edit (Some(original name)) a contact.
    Contact(Option<String>),
    /// Name the person behind a conversation: their payment code is known, and so is the account
    /// that wrote the message you were reading.
    ContactFromPeer {
        code: String,
        address: Option<String>,
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

/// Review confirmation state.
pub struct ReviewState {
    pub review: Review,
    pub scroll: u16,
    pub content_lines: u16,
    pub viewport: u16,
    pub approve_focused: bool,
    pub opened: Instant,
}

impl ReviewState {
    /// Approval enables only once the whole review has been scrolled into view and shortly after opening.
    pub fn can_approve(&self) -> bool {
        let seen_all = self.scroll + self.viewport >= self.content_lines;
        seen_all && self.opened.elapsed().as_millis() > 400
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
        let resolve = |id: &str| super::theme::resolve(app.paths.root(), id, app.light_hint, app.no_color).0;
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
    SwitchNetwork(String),
    /// Register an announced sender's channel.
    AcceptOffer(String),
    /// Decline an announced sender's channel, for good.
    DeclineOffer(String),
}

pub enum Modal {
    Orders {
        rows: Vec<wallet_core::plans::TradePlan>,
        selected: usize,
    },
    None,
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
    pub error: bool,
    pub at: Instant,
}

/// Onboarding steps when no wallets exist.
pub enum Onboarding {
    Theme(Picker),
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

/// A password being checked on its own thread: the wallet it is for, and the answer (the keys and
/// the password, or why not).
pub type UnlockCheck = (String, std::sync::mpsc::Receiver<Result<(wallet_core::identity::Unlocked, Zeroizing<String>), String>>);

/// Background wallet creation (Argon2 runs off the render thread).
pub type Creation = std::sync::mpsc::Receiver<Result<(WalletMeta, Option<Zeroizing<String>>), String>>;

/// Palette entries.
#[derive(Clone, Debug)]
pub struct Action {
    pub label: &'static str,
    pub key: &'static str,
    pub cli: &'static str,
    pub id: &'static str,
}

macro_rules! action {
    ($label:literal, $key:literal, $cli:literal, $id:literal) => {
        Action { label: $label, key: $key, cli: $cli, id: $id }
    };
}

pub const ACTIONS: &[Action] = &[
    action!("Send QUAI", "s", "quai-terminal send quai --to ADDR --amount N", "send_quai"),
    action!("Send Qi", "1 ] s", "quai-terminal send qi --to CODE --amount N", "send_qi"),
    action!("Send token", "", "quai-terminal send token SYMBOL --to ADDR --amount N", "send_token"),
    action!("Receive QUAI", "r", "quai-terminal receive --qr", "receive_quai"),
    action!("Receive Qi (payment code)", "1 ] r", "quai-terminal receive --asset qi --qr", "receive_qi"),
    action!("Portfolio (tokens, prices, value)", "1", "quai-terminal portfolio", "portfolio"),
    action!("Home", "1", "quai-terminal portfolio --history", "home"),
    action!("Swap tokens (Quainance)", "t · 2", "quai-terminal swap FROM TO --amount N", "swap"),
    action!("Convert QUAI → Qi", "c", "quai-terminal convert quai-to-qi --amount N", "convert_quai_qi"),
    action!("Convert Qi → QUAI", "C", "quai-terminal convert qi-to-quai --amount N", "convert_qi_quai"),
    action!("Conversion quote & risk", "2 ]]] enter", "quai-terminal convert quote quai-to-qi N", "quote"),
    action!("Wrap Qi → WQI", "2 ]]]]", "quai-terminal wrap qi --amount N", "wrap_qi"),
    action!("Claim WQI", "2 ]]]]", "quai-terminal wrap claim", "claim_wqi"),
    action!("Unwrap WQI → Qi", "2 ]]]]", "quai-terminal wrap unwrap-qi --amount N", "unwrap_wqi"),
    action!("Wrap QUAI → WQUAI", "2 ]]]]", "quai-terminal wrap quai --amount N", "wrap_quai"),
    action!("Unwrap WQUAI → QUAI", "2 ]]]]", "quai-terminal wrap unwrap-quai --amount N", "unwrap_quai"),
    action!("NFTs you hold", "3", "quai-terminal nft list", "nfts"),
    action!("Explore NFT collections", "3 ]", "quai-terminal market collections", "explore"),
    action!("NFT listings (Bazarr)", "3 ]]", "quai-terminal market listings", "listings"),
    action!("Approve token spender", "", "quai-terminal token approve SYMBOL SPENDER --amount N", "approve"),
    action!("Import token", "1 i", "quai-terminal token import ADDRESS", "import_token"),
    action!("Discover tokens you hold", "1 D", "quai-terminal token discover --import", "discover_tokens"),
    action!("Add Quai account", "1 ]] a", "quai-terminal account add", "add_account"),
    action!("New Qi / mining address", "1 ] a", "quai-terminal mining new", "new_qi_address"),
    action!("Scan Qi (gap 50)", "1 ] S", "quai-terminal qi scan", "scan_qi"),
    action!("Deep scan Qi", "1 ] D", "quai-terminal qi scan --deep N", "deep_scan"),
    action!("Consolidate Qi (aggregate small coins)", "1 ] A", "quai-terminal qi consolidate --aggregate", "aggregate"),
    action!("Sweep Qi (keep denominations)", "1 ] W", "quai-terminal qi consolidate", "sweep"),
    action!("Time locks", "1 ]]]", "quai-terminal locks", "locks"),
    action!("Discover payments (mailbox)", "4 d", "quai-terminal payment discover", "discover"),
    action!("Add payment peer", "4 p", "quai-terminal payment add CODE", "add_peer"),
    action!("Notify payment peer", "", "quai-terminal payment notify PEER", "notify"),
    action!("Contacts (addresses & payment codes)", "4", "quai-terminal contact list", "contacts"),
    action!("Add contact", "4 a", "quai-terminal contact add NAME --address ADDR", "add_contact"),
    action!("Launches (Quainance launch zone)", "2 ]]]]]", "quai-terminal pool launches", "launches"),
    action!("Speed up selected transaction", "5 u", "quai-terminal tx speedup ID", "speedup"),
    action!("Fill nonce gap", "", "quai-terminal tx fill-gap --from ACCOUNT", "fill_gap"),
    action!("Data sources", "0 ]]", "quai-terminal data status", "data_sources"),
    action!("Test data connections", "0 ]] t", "quai-terminal data test", "test_data"),
    action!("Theme showroom", "T", "quai-terminal theme list", "themes"),
    action!("Glossary: what the words mean", "? g", "", "glossary"),
    action!("Unlock this wallet in the daemon", "", "quai-terminal daemon unlock -w NAME", "daemon_unlock"),
    action!("Lock screen gallery", "L", "quai-terminal config set lock_effect NAME", "lock_gallery"),
    action!("Refresh everything", "R", "quai-terminal balance", "refresh"),
    action!("Lock wallet", "l", "", "lock"),
    action!("Reveal recovery phrase", "", "quai-terminal wallet export-mnemonic", "export_phrase"),
    action!("Encrypted backup", "", "quai-terminal wallet backup FILE", "backup"),
    action!("Switch network", "0 enter", "quai-terminal network use ID", "network"),
    action!("Notifications", "N", "quai-terminal notifications", "notifications"),
    action!("Enter the matrix", "", "", "matrix"),
    action!("PoEM · proof of entropy minima (:poem)", "", "", "poem"),
    action!("Help", "?", "quai-terminal --help", "help"),
    action!("Quit", "q", "", "quit"),
];

pub struct App {
    pub paths: wallet_core::paths::Paths,
    pub registry: wallet_core::registry::Registry,
    pub network_id: String,
    pub qr_rect: Option<(ratatui::layout::Rect, String)>,
    pub pending_unlock: Option<Zeroizing<String>>,
    pub config: AppConfig,
    pub theme: Theme,
    pub light_hint: bool,
    pub no_color: bool,
    pub caps: Caps,
    pub meta: Option<WalletMeta>,
    /// Wallets on this computer, for the Wallets screen (re-read when it opens).
    pub wallets: Vec<WalletMeta>,
    /// Each wallet's last priced summary on this network (System › Wallets).
    pub wallet_summaries: HashMap<String, wallet_core::cockpit::WalletSummary>,
    /// Each wallet's QUAI read live from its public addresses.
    pub wallet_quai: HashMap<String, wallet_core::sdk::U256>,
    pub worker: Option<Worker>,
    pub dash: Dashboard,
    pub screen: Screen,
    pub modal: Modal,
    pub onboarding: Option<Onboarding>,
    pub creating: Option<Creation>,
    /// A monitoring endpoint being verified (network id, endpoint, outcome).
    pub monitor_check: Option<std::sync::mpsc::Receiver<MonitorCheck>>,
    /// An IPFS gateway being tested before it is saved.
    pub ipfs_check: Option<std::sync::mpsc::Receiver<IpfsCheck>>,
    /// The destination the send form has asked about and is still waiting on.
    pub contract_probe: Option<String>,
    /// What the last answered probe found, kept while the form that asked is open.
    pub contract_found: Option<wallet_core::contracts::Discovered>,
    /// A wallet switch is waiting for the new wallet's accounts before the open view can ask for
    /// anything. Set when the switch starts, cleared by the first dashboard that has accounts.
    pub reload_view_on_accounts: bool,
    /// Destinations already asked about on this form, and whether an answer came back at all.
    ///
    /// Without this a probe that fails — an unresolvable address, the node down, a Qi destination
    /// — clears both fields above, so the next keystroke sees nothing remembered and asks again,
    /// and the form turns into one chain read per keypress for as long as it stays open.
    pub contract_asked: std::collections::HashSet<String>,
    pub selected: usize,
    pub jump_pending: Option<char>,
    pub toasts: Vec<Toast>,
    pub busy: Option<String>,
    pub locked: bool,
    pub lock_input: String,
    pub last_input: Instant,
    pub quit: bool,
    /// Unlock decrypt ceremony (drawn over the main area, skippable).
    pub ceremony: Option<Ceremony>,
    /// Whole-main-area ambient effect (lock screen, easter egg).
    pub ambient: Option<Ceremony>,
    /// The frame a finished lock effect left behind, dissolving under the one that replaced it.
    pub lock_fade: Option<(String, Instant)>,
    /// Corner celebration stamp.
    pub celebration: Option<Ceremony>,
    pub transition: Option<tachyonfx::Effect>,
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
    pub kitty: KittyGraphics,
    pub dirty: bool,
    pub last_frame: Instant,
    pub pending_theme_reload: bool,
    pub bell: bool,
    pub beat: Option<Instant>,
    pub unlocked_at: Option<Instant>,
    /// Text to hand to the terminal clipboard (OSC 52) after the next frame.
    pub clipboard: Option<String>,
    /// Recent messages (toasts disappear; this keeps them readable).
    pub log: std::collections::VecDeque<Toast>,
    /// A non-secret form set aside by auto-lock, restored after unlock.
    pub parked: Option<Form>,
    pub lock_error: Option<String>,
    /// An unlock is in flight. The vault's KDF is deliberately slow, so the lock screen says so
    /// while the password is checked.
    pub unlocking: bool,
    /// When that unlock was submitted, so one that never answers is given up on.
    pub unlocking_since: Option<Instant>,
    /// The password check itself. It runs on its own thread, never on the worker's queue: the
    /// worker may be in a sync step that cannot stop (a Qi refresh), and an unlock that waited for
    /// it could take a minute.
    pub unlock_check: Option<UnlockCheck>,
    /// A wallet switch locked the screen before the worker reached it. The worker's own `Locked`
    /// for that switch arrives later and must not lock a wallet unlocked in the meantime.
    pub switch_lock_pending: bool,
    pub lock_warned: bool,
    /// Session-only theme (QUAI_TERMINAL_THEME); cleared when a theme is chosen.
    pub theme_override: Option<String>,
    /// Session-only plain mode (NO_COLOR, Linux console): no block digits, reduced motion.
    pub plain: bool,
    /// Focused pane within the view (Tab / Shift-Tab).
    pub pane: usize,
    /// The terminal window has focus (ambient light pauses without it).
    pub focused: bool,
    /// Last sub-tab per section.
    pub section_tabs: [usize; 6],
    /// Markets and the swap card are drawn side by side (the trader layout, at this width).
    pub trader: bool,
    /// The pinned chat was drawn this frame, so Tab can reach it.
    pub dock_shown: bool,
    /// The pinned chat has the keyboard: typing goes into its message box.
    pub dock_focus: bool,
    /// What is being written in the pinned chat, kept while focus is elsewhere.
    pub dock_draft: String,
    /// A password on its way to the daemon: the answer (wallet name, or why not).
    pub handoff: Option<std::sync::mpsc::Receiver<std::result::Result<String, String>>>,
    /// Palette entries chosen lately, newest first (`palette::Entry::key`).
    pub palette_recent: Vec<String>,
    /// Activity filter tab.
    pub activity_filter: ActivityFilter,
    /// Detail stack (Enter pushes, Esc pops).
    pub detail: Vec<Detail>,
    /// Selection inside the top detail view (collection items, listings).
    pub detail_selected: usize,
    /// Ecosystem data: portfolio, images, swaps, NFTs.
    pub eco: super::eco::Eco,
    /// Background data worker (explorer, prices, images, quotes).
    pub data: Option<super::data::DataWorker>,
    /// Kind of the review being committed (follow-ups after submission).
    pub committing_kind: Option<String>,
    /// The help overlay shows the one-time "what moved" note.
    pub help_moved: bool,
}

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
            qr_rect: None,
            pending_unlock: None,
            config,
            theme,
            light_hint: false,
            no_color: false,
            caps,
            onboarding: if meta.is_none() { Some(Onboarding::Choose { selected: 0 }) } else { None },
            creating: None,
            wallets: Vec::new(),
            wallet_summaries: HashMap::new(),
            wallet_quai: HashMap::new(),
            monitor_check: None,
            ipfs_check: None,
            reload_view_on_accounts: false,
            contract_probe: None,
            contract_found: None,
            contract_asked: Default::default(),
            locked: signable,
            meta,
            worker: None,
            dash: Dashboard::default(),
            screen: Screen::Home,
            modal: Modal::None,
            selected: 0,
            jump_pending: None,
            toasts: Vec::new(),
            busy: None,
            lock_input: String::new(),
            last_input: Instant::now(),
            quit: false,
            ceremony: None,
            ambient: None,
            lock_fade: None,
            celebration: None,
            transition: None,
            edge_intro: None,
            beat_order: 2,
            recent_hashes: std::collections::VecDeque::new(),
            row_flash: HashMap::new(),
            drawer_flash: HashMap::new(),
            poem_haiku: None,
            kitty: KittyGraphics::default(),
            dirty: true,
            last_frame: Instant::now(),
            pending_theme_reload: false,
            bell: false,
            beat: None,
            unlocked_at: None,
            clipboard: None,
            log: std::collections::VecDeque::new(),
            parked: None,
            lock_error: None,
            unlocking: false,
            unlocking_since: None,
            unlock_check: None,
            switch_lock_pending: false,
            lock_warned: false,
            theme_override: None,
            plain: false,
            pane: 0,
            focused: true,
            section_tabs: [0; 6],
            trader: false,
            dock_shown: false,
            dock_focus: false,
            dock_draft: String::new(),
            handoff: None,
            palette_recent: Vec::new(),
            activity_filter: ActivityFilter::All,
            detail: Vec::new(),
            detail_selected: 0,
            eco,
            data: None,
            committing_kind: None,
            help_moved: false,
        }
    }

    /// First-run onboarding starts in the theme showroom.
    pub fn start_onboarding(&mut self) {
        if self.meta.is_none() {
            self.onboarding = Some(Onboarding::Theme(Picker::new(self)));
        }
    }

    pub fn motion(&self) -> Motion {
        if (self.caps.ssh || self.plain) && self.config.motion.effects() { Motion::Reduced } else { self.config.motion }
    }

    fn effects_allowed(&self) -> bool {
        self.motion().effects() && self.config.ceremonies
    }

    pub fn animating(&self) -> bool {
        self.ceremony.is_some()
            || self.transition.is_some()
            || self.edge_intro.is_some_and(|s| s.elapsed().as_millis() < super::edge::INTRO_TOTAL_MS)
            || self.row_flash.values().any(|s| s.elapsed().as_millis() < super::edge::FLASH_MS)
            || self.drawer_flash.values().any(|s| s.elapsed().as_millis() < super::edge::FLASH_MS)
            || self.ambient.is_some()
            || self.celebration.is_some()
            || matches!(self.modal, Modal::Effects(_))
    }

    pub fn send(&self, cmd: Cmd) {
        if let Cmd::Quote { direction, amount } = cmd {
            let key = self.eco.convert.protocol_key.get().wrapping_add(1).max(1);
            self.eco.convert.protocol_key.set(key);
            self.send_data(super::data::DataCmd::ProtocolQuote { key, direction, amount, card: self.screen == Screen::Convert });
            return;
        }
        if let Some(w) = &self.worker {
            w.send(cmd);
        }
    }

    pub fn toast(&mut self, text: impl Into<String>, error: bool) {
        let toast = Toast { text: text.into(), error, at: Instant::now() };
        self.log.push_front(toast.clone());
        self.log.truncate(50);
        self.toasts.push(toast);
        if self.toasts.len() > 4 {
            self.toasts.remove(0);
        }
        self.dirty = true;
    }

    /// Transactions that are sent and not yet mined, oldest first. They are what the corner
    /// pill counts down; settling and locked operations wait on the destination, not the miner.
    pub fn confirming_ops(&self) -> Vec<&wallet_core::appdb::Operation> {
        use wallet_core::appdb::OpStatus;
        let mut open: Vec<&wallet_core::appdb::Operation> =
            self.dash.ops.iter().filter(|o| matches!(o.status, OpStatus::Submitted | OpStatus::Unknown)).collect();
        open.sort_by_key(|o| o.updated);
        open
    }

    pub fn start_transition(&mut self) {
        // Borders draw themselves in (see `edge`); content appears at once, so amounts never animate.
        if self.motion().effects() {
            self.edge_intro = Some(Instant::now());
        }
    }

    /// Lock now: the screen locks immediately, and the worker drops the keys as soon as it
    /// reaches the command (it may still be finishing a network sync).
    fn lock_now(&mut self, size: Option<(u16, u16)>) {
        if self.can_sign() && !self.locked {
            self.send(Cmd::Lock);
            self.enter_lock(size);
            // A wallet handed to the daemon is locked there too; off the UI thread, since the daemon
            // answers between polls.
            if let Some(wallet) = self.meta.as_ref().map(|m| m.id.clone())
                && crate::daemon::state(&self.paths).is_some_and(|d| d.unlocked(&wallet))
            {
                let paths = self.paths.clone();
                let _ = std::thread::Builder::new().name("daemon-lock".into()).spawn(move || {
                    let _ = crate::daemon::lock_wallet(&paths, &wallet);
                });
            }
        }
    }

    /// Switch the UI to the lock screen. Idempotent: the worker's `Locked` confirmation after a
    /// local lock changes nothing (and doesn't restart the animation).
    pub(super) fn enter_lock(&mut self, size: Option<(u16, u16)>) {
        if self.locked {
            return;
        }
        self.locked = true;
        self.flow_on_lock();
        // Keep a half-filled, non-secret form; everything else is dropped with the keys.
        if let Modal::Form(form) = std::mem::replace(&mut self.modal, Modal::None)
            && !form.fields.iter().any(|f| f.is_secret())
        {
            let mut form = form;
            form.pending = false;
            self.parked = Some(form);
        }
        self.dash.unlocked = false;
        self.dash.peers.clear();
        self.dash.offers.clear();
        self.ceremony = None;
        self.celebration = None;
        self.lock_fade = None;
        self.unlocked_at = None;
        self.lock_input.zeroize();
        // Without a size the next tick starts the animation.
        if let Some(size) = size {
            self.start_lock_ceremony(size);
        }
    }

    /// Start (or loop) the lock screen animation on a canvas exactly the size of the art area.
    pub fn start_lock_ceremony(&mut self, size: (u16, u16)) {
        if self.effects_allowed() && self.locked {
            let (w, h) = lock_art_size(size);
            let chosen = self.config.lock_effect.as_str();
            let effect = if chosen != "random" && super::fx::EFFECTS.iter().any(|(n, _)| *n == chosen) {
                chosen.to_string()
            } else {
                super::fx::random_lock_effect().to_string()
            };
            let args = super::fx::theme_args(&effect, &self.theme);
            // Public chain data only: the lock screen never shows wallet data.
            let text = match self.chain_weather() {
                Some(line) => format!("{}\n\n{line}", super::fx::wordmark_block()),
                None => super::fx::wordmark_block(),
            };
            // 900 frames at LOCK_SPEED is about ten seconds, which reads as a flourish rather
            // than a wait.
            self.ambient = Ceremony::with_args(&effect, &args, &text, w, h, 900).map(|c| c.at_speed(super::fx::LOCK_SPEED));
        }
    }

    /// Lock-screen line from public chain data: height, head hash and local time (ASCII).
    pub fn chain_weather(&self) -> Option<String> {
        let h = self.dash.health.as_ref()?;
        let hash = h.head_hash.trim_start_matches("0x");
        let short = if hash.len() > 12 { format!("0x{}..{}", &hash[..6], &hash[hash.len() - 4..]) } else { format!("0x{hash}") };
        let kind = match h.order {
            Some(0) => "prime",
            Some(1) => "region",
            _ => "zone",
        };
        let time = chrono::Local::now().format("%H:%M");
        Some(format!("#{}  {short}  {kind}  {time}", wallet_core::amount::group_thousands(&h.height.to_string())))
    }

    /// Corner stamp for good news. Money values are shown in the toast, never animated.
    fn celebrate(&mut self, word: &str, big: bool) {
        if self.config.sound {
            self.bell = true;
        }
        if !self.effects_allowed() {
            return;
        }
        let effect = if big { "fireworks" } else { "rings" };
        let args = super::fx::theme_args(effect, &self.theme);
        let (w, h) = if big { (44, 11) } else { (30, 7) };
        self.celebration = Ceremony::with_args(effect, &args, &super::fx::seal(word), w, h, if big { 160 } else { 110 });
    }

    /// Handle a worker event.
    pub fn on_event(&mut self, ev: Ev, size: (u16, u16)) {
        self.dirty = true;
        match ev {
            Ev::SplitQuote { key, result } => {
                if self.screen != Screen::Swap || self.eco.split_request != self.swap_input_key().map(|identity| (key, identity)) {
                    return;
                }
                self.eco.split_request = None;
                match result {
                    Ok(result) => match result.plan {
                        Some(plan) => {
                            let Some(account) = self.dash.accounts.first().map(|a| a.address.clone()) else {
                                return;
                            };
                            let intent = wallet_core::execution::TradingIntent {
                                account,
                                max_fee: None,
                                action: wallet_core::execution::TradingAction::Split {
                                    plan: Box::new(plan),
                                    index: 0,
                                    deadline: self.eco.swap.deadline_minutes,
                                },
                            };
                            self.start_flow(super::eco::FlowKind::Steps {
                                prepare: Box::new(Prepare::Trading { intent }),
                                label: "split swap · separate allocations".into(),
                            });
                        }
                        None => self.toast(result.reason, false),
                    },
                    Err(error) => self.toast(friendly_error(&error), true),
                }
            }
            Ev::QiMax { key, result } => {
                if self.eco.max_request != Some((key, self.max_identity())) {
                    return;
                }
                self.eco.max_request = None;
                match result {
                    Ok(q) => {
                        if self.screen == Screen::Convert {
                            self.eco.convert.amount = q.amount;
                            self.eco.convert.edited = Some(Instant::now());
                            self.eco.convert.quote = None;
                        } else if self.screen == Screen::Wrap {
                            self.eco.wrap.amount = q.amount;
                        }
                        self.toast(
                            format!(
                                "amount quoted at one-qit resolution with {} qits fee; {} inputs excluded. Preparation refreshes fees",
                                q.fee_qits, q.excluded_inputs
                            ),
                            false,
                        );
                    }
                    Err(e) => self.toast(friendly_error(&e), true),
                }
            }
            // What a send destination turned out to be. A late answer for an address that has
            // since been edited is dropped rather than shown against the wrong one.
            Ev::Contract { address, found } => {
                if self.contract_probe.as_deref() == Some(address.as_str()) {
                    self.contract_probe = None;
                    self.contract_found = *found;
                    self.refresh_form_note();
                }
            }
            Ev::Dashboard(mut d) => {
                // Channels is chosen by payment code: keep the cursor on the same one when offers
                // arrive, leave or move, so a key never lands on a different sender.
                let anchor = (self.screen == Screen::Channels)
                    .then(|| self.channel_offer().map(|o| o.code.clone()).or_else(|| self.channel_peer().map(|p| p.code.clone())))
                    .flatten();
                // The pending lane may have read the journal after this refresh did: keep its read.
                if d.ops_at < self.dash.ops_at
                    && d.network_id == self.dash.network_id
                    && d.meta.as_ref().map(|m| &m.id) == self.dash.meta.as_ref().map(|m| &m.id)
                {
                    d.ops = self.dash.ops.clone();
                    d.ops_at = self.dash.ops_at;
                }
                self.observe_changes(&d);
                self.dash = *d;
                if self.locked {
                    // A refresh that finished after a local lock must not bring unlocked-only data back.
                    self.dash.unlocked = false;
                    self.dash.peers.clear();
                    self.dash.offers.clear();
                }
                if let Some(m) = &self.dash.meta {
                    self.meta = Some(m.clone());
                }
                // A wallet switch clears every cached view, but the screen the user is on was
                // never re-opened, so nothing asked for the new wallet's data — and at the moment
                // of the switch there were no accounts to ask about yet. The first dashboard that
                // brings them is when the open view can load, so it is re-opened here.
                if self.reload_view_on_accounts && !self.dash.accounts.is_empty() {
                    self.reload_view_on_accounts = false;
                    self.on_view_opened();
                }
                if let Some(code) = anchor
                    && let Some(i) =
                        self.dash.offers.iter().map(|o| &o.code).chain(self.dash.peers.iter().map(|p| &p.code)).position(|c| *c == code)
                {
                    self.selected = i;
                }
                // Balances changed (a swap output, a claimed WQI, a transfer): rebuild the portfolio
                // on the views that show it, without waiting for the view to be reopened.
                if matches!(self.screen, Screen::Home | Screen::Swap) || self.eco.portfolio.is_none() {
                    self.maybe_refresh_portfolio(false);
                }
                self.preload();
            }
            // Results the worker produced before it saw a pending lock are dropped with the keys.
            Ev::Orders { wallet, network, rows } => {
                if self.meta.as_ref().is_some_and(|m| m.id == wallet) && self.dash.network_id == network {
                    if matches!(
                        self.modal,
                        Modal::None | Modal::Orders { .. } | Modal::Form(Form { kind: FormKind::OrderCreate { .. }, .. })
                    ) {
                        let selected = if let Modal::Orders { selected, .. } = &self.modal { *selected } else { 0 };
                        self.modal = Modal::Orders { selected: selected.min(rows.len().saturating_sub(1)), rows };
                    } else {
                        self.toast("order state updated; L on Swap reopens orders", false);
                    }
                }
            }
            Ev::OrderReview(r) => {
                if self.locked || self.eco.flow.is_some() || matches!(self.modal, Modal::Review(_)) {
                    self.send(Cmd::Discard(r.op_id.clone()));
                    self.toast("order review discarded because the wallet locked or another review began", true);
                } else {
                    self.modal = Modal::Review(ReviewState {
                        review: *r,
                        scroll: 0,
                        content_lines: 1,
                        viewport: 1,
                        approve_focused: false,
                        opened: Instant::now(),
                    });
                }
            }
            Ev::Review(r) if self.locked => self.send(Cmd::Discard(r.op_id.clone())),
            Ev::Secret(_) | Ev::Quote(_) if self.locked => {}
            Ev::Review(r) => {
                if !self.flow_on_review(&r.op_id) {
                    self.send(Cmd::Discard(r.op_id.clone()));
                    return;
                }
                self.modal = Modal::Review(ReviewState {
                    review: *r,
                    scroll: 0,
                    content_lines: 1,
                    viewport: 1,
                    approve_focused: false,
                    opened: Instant::now(),
                });
            }
            Ev::Submitted(s) => {
                let kind = self.committing_kind.take();
                if let Some(kind) = &kind {
                    self.after_submit(kind);
                }
                // A step in a sequence continues on its own; only the last step shows the result.
                if !self.flow_on_submitted(&s.op_id, kind.as_deref().unwrap_or_default()) && !self.locked {
                    self.modal = Modal::Result(s);
                }
            }
            Ev::Quote(q) if self.screen == Screen::Convert => {
                let card = &self.eco.convert;
                let direction = if card.qi_to_quai { "qi_to_quai" } else { "quai_to_qi" };
                let decimals = if card.qi_to_quai { wallet_core::amount::QI_DECIMALS } else { 18 };
                if q.direction != direction
                    || wallet_core::amount::parse_amount(&card.amount, decimals).ok().map(|v| v.to_string()).as_deref()
                        != Some(q.amount.as_str())
                {
                    return;
                }
                // The right tolerance depends on the size, and the card cannot know it before the
                // quote arrives: it starts unset, which ConversionSlippage rejects outright, and a
                // fixed 3% would be refunded at 250 QUAI and far more than needed at 50. Adopt the
                // quote's suggestion until the user picks their own.
                if !self.eco.convert.manual_slippage {
                    self.eco.convert.slippage_bps = q.suggested_slippage_bps;
                }
                self.eco.convert.quote = Some(*q);
            }
            Ev::Quote(q) => self.modal = Modal::Quote(q),
            Ev::Info(m) => self.toast(m, false),
            Ev::Ack(m) => {
                if matches!(&self.modal, Modal::Form(f) if f.pending) {
                    self.modal = Modal::None;
                }
                self.toast(m, false);
            }
            Ev::CommitError { op_id, message } => {
                if self.eco.flow.as_ref().is_some_and(|flow| flow.review_op.as_deref() == Some(&op_id)) {
                    self.flow_on_rejected(&op_id);
                }
                self.committing_kind = None;
                self.send(Cmd::Journal);
                self.toast(format!("{}; inspect transaction {} before retrying", friendly_error(&message), op_id), true);
            }
            Ev::Error(m) => {
                self.flow_on_error();
                let text = friendly_error(&m);
                if let Modal::Form(form) = &mut self.modal
                    && form.pending
                {
                    form.pending = false;
                    form.error_field = error_field(form, &text);
                    if let Some(i) = form.error_field {
                        form.focus = i;
                    }
                    form.error = Some(text);
                } else if self.locked {
                    // Background work failing while locked (the node, a sync). A refused password
                    // is not reported here: the lock screen checks passwords itself.
                    self.log.push_front(Toast { text, error: true, at: Instant::now() });
                } else {
                    self.toast(text, true);
                }
            }
            Ev::Busy(b) => self.busy = b,
            // A watch-only wallet opened by a switch: nothing to unlock.
            Ev::Unlocked => self.show_unlocked(),
            Ev::Locked => {
                // The confirmation of a switch this screen already locked for. If that wallet was
                // unlocked while the worker was still getting there, it stays unlocked.
                if !(std::mem::take(&mut self.switch_lock_pending) && !self.locked) {
                    self.enter_lock(Some(size));
                }
            }
            Ev::KeysRefused => {
                self.switch_lock_pending = false;
                self.enter_lock(Some(size));
                self.lock_error = Some("that wallet did not open — unlock it again".into());
            }
            Ev::Secret(text) => {
                self.modal = Modal::Secret { text, title: "Anyone with these words controls your funds".into() };
            }
            Ev::Conversation { peer, result } => {
                if self.eco.board.dm_loading.as_deref() == Some(peer.as_str()) {
                    self.eco.board.dm_loading = None;
                }
                self.eco.board.dm_at.insert(peer.clone(), Instant::now());
                if result.is_ok() || !matches!(self.eco.board.dms.get(&peer), Some(Ok(_))) {
                    self.eco.board.dms.insert(peer, result);
                }
            }
            Ev::Notify { title, body } => self.toast(format!("{title}: {body}"), false),
            Ev::Chat { subs, pin, note } => {
                self.eco.board.subs = subs;
                self.eco.board.pin = pin;
                if let Some(note) = note {
                    self.toast(note, false);
                }
            }
            // A sent transaction was mined: show it now, through the same path as a refresh so a
            // confirmation celebrates and a sequence moves on to its next step.
            Ev::Ops { wallet, network, ops, at } => {
                if at > self.dash.ops_at && network == self.dash.network_id && self.dash.meta.as_ref().is_some_and(|m| m.id == wallet) {
                    let mut next = self.dash.clone();
                    next.ops = ops;
                    next.ops_at = at;
                    self.on_event(Ev::Dashboard(Box::new(next)), size);
                }
            }
            Ev::ChatNews(news) => {
                // A chat already on screen (open on the Board, or pinned) is being read; the rest also
                // goes to the desktop. Notice titles are the chat's label (`#general`, `Alice · sealed`).
                let open = (self.screen == Screen::Board).then(|| self.board_row().map(|r| App::chat_target(&r).0)).flatten();
                let on_screen: Vec<String> = open.iter().chain(self.eco.board.pin.iter()).map(|t| self.chat_label(t)).collect();
                for (title, body) in news {
                    let visible = on_screen.iter().any(|l| title == *l || title.starts_with(&format!("{l} ·")));
                    if self.config.notifications && !visible {
                        crate::notify::desktop(&title, &body);
                    }
                    self.toast(format!("{title} · {body}"), false);
                }
            }
        }
    }

    /// Heartbeat on new blocks; celebrate confirmations and new receipts (not on first load).
    fn observe_changes(&mut self, next: &Dashboard) {
        let old_height = self.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
        if let Some(h) = &next.health
            && h.height > old_height
        {
            if old_height > 0 {
                self.beat = Some(Instant::now());
                self.beat_order = h.order.unwrap_or(2);
            }
            if self.recent_hashes.back() != Some(&h.head_hash) {
                self.recent_hashes.push_back(h.head_hash.clone());
                while self.recent_hashes.len() > 24 {
                    self.recent_hashes.pop_front();
                }
            }
        }
        if self.dash.refreshed_at == 0 || self.dash.network_id != next.network_id {
            return;
        }
        let now = Instant::now();
        self.row_flash.retain(|_, s| s.elapsed().as_millis() < super::edge::FLASH_MS);
        self.drawer_flash.retain(|_, s| s.elapsed().as_millis() < super::edge::FLASH_MS);
        if self.motion().effects() {
            // Confirmations: a row lights once when it reaches the target.
            let new_height = next.health.as_ref().map(|h| h.height).unwrap_or(old_height);
            for op in &next.ops {
                let before = super::ui::confirmations(op, old_height).map(|(n, _)| n).unwrap_or(0);
                let after = super::ui::confirmations(op, new_height).map(|(n, _)| n).unwrap_or(0);
                if old_height > 0 && before < super::ui::CONFIRM_TARGET && after >= super::ui::CONFIRM_TARGET {
                    self.row_flash.insert(op.id.clone(), now);
                }
            }
            // Cash drawer: each newly arrived Qi coin lights its denomination slot.
            if let (Some(old), Some(new)) = (&self.dash.qi, &next.qi) {
                for coin in new.coins.iter().filter(|c| !old.coins.iter().any(|o| o.outpoint == c.outpoint)) {
                    self.drawer_flash.insert(coin.denomination, now);
                }
            }
        }
        let confirmed: Vec<String> = next
            .ops
            .iter()
            .filter(|op| {
                matches!(op.status, OpStatus::Confirmed | OpStatus::Settled)
                    && self.dash.ops.iter().any(|o| o.id == op.id && o.status != op.status && !o.status.is_terminal())
            })
            .map(|op| op.kind.clone())
            .collect();
        for kind in &confirmed {
            self.after_confirm(kind);
        }
        let finished = !confirmed.is_empty();
        let newest_seen = self.dash.notifications.iter().map(|n| n.id).max().unwrap_or(0);
        if next.notifications.iter().any(|n| n.id > newest_seen && n.title == "NFT sold") {
            self.eco.nfts = None;
            self.load_my_listings();
            self.celebrate("SOLD", true);
            return;
        }
        let received = next.activity.iter().any(|a| !self.dash.activity.iter().any(|o| o.key == a.key));
        if received && !self.config.first_receive_celebrated {
            self.config.first_receive_celebrated = true;
            self.save_config();
            self.celebrate("FIRST RECEIPT", true);
        } else if received {
            self.celebrate("RECEIVED", false);
        } else if finished {
            let settled = next
                .ops
                .iter()
                .any(|op| op.status == OpStatus::Settled && self.dash.ops.iter().any(|o| o.id == op.id && o.status != op.status));
            self.celebrate(if settled { "SETTLED" } else { "CONFIRMED" }, false);
        }
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::ScrollUp => self.move_selection(-1),
            _ => {}
        }
    }

    /// Bracketed paste into whichever text field has focus.
    pub fn on_paste(&mut self, text: &str) {
        let clean: String = text.trim().chars().filter(|c| !c.is_control()).take(1024).collect();
        match &mut self.modal {
            Modal::Form(form) if !form.pending => {
                if let Some(f) = form.fields.get_mut(form.focus)
                    && !matches!(f.kind, FieldKind::Choice(_))
                {
                    f.value.push_str(&clean);
                }
            }
            Modal::Palette { query, .. } => query.push_str(&clean),
            Modal::None if self.locked => self.lock_input.push_str(&clean),
            _ => {}
        }
        if let Some(Onboarding::Details { fields, focus, .. }) = &mut self.onboarding
            && let Some(f) = fields.get_mut(*focus)
        {
            f.value.push_str(&clean);
        }
        self.dirty = true;
    }

    pub fn move_selection(&mut self, delta: i64) {
        if let Modal::Review(r) = &mut self.modal {
            let max = r.content_lines.saturating_sub(r.viewport);
            r.scroll = (r.scroll as i64 + delta).clamp(0, max as i64) as u16;
            self.dirty = true;
            return;
        }
        if !self.detail.is_empty() {
            let len = self.detail_len();
            if len > 0 {
                self.detail_selected = (self.detail_selected as i64 + delta).rem_euclid(len as i64) as usize;
            }
            self.dirty = true;
            return;
        }
        let len = self.list_len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected as i64 + delta).rem_euclid(len as i64) as usize;
        self.dirty = true;
    }

    /// Rows in the current screen's primary list.
    pub fn list_len(&self) -> usize {
        match self.screen {
            Screen::Home if self.pane == 1 => self.activity_rows().len().min(12),
            Screen::Home => self.eco.portfolio.as_ref().map_or(0, |p| p.rows.len()),
            Screen::Pools => self.eco.pools_view.positions.as_ref().and_then(|r| r.as_ref().ok()).map_or(0, Vec::len),
            Screen::Accounts => self.dash.accounts.len(),
            Screen::Activity => self.activity_rows().len(),
            Screen::Qi => self.dash.qi.as_ref().map_or(0, |q| q.coins.len()),
            Screen::Board if self.pane == 1 => self.board_message_count(),
            Screen::Board => self.board_rows().len(),
            Screen::Wallets => self.wallets.len(),
            Screen::Channels => self.dash.offers.len() + self.dash.peers.len(),
            Screen::Contacts => self.dash.contacts.len(),
            Screen::Locks => self.dash.locks.len(),
            Screen::Launches => self.launch_rows().len(),
            Screen::Network => self.dash.networks.len(),
            Screen::Settings => SETTINGS.len(),
            Screen::DataSources => DATA_SOURCES.len(),
            Screen::Collected => self.eco.nft_len(),
            Screen::Explore => self.eco.collections_filtered().len(),
            Screen::Listings => self.eco.listings_len(),
            Screen::Markets if self.pane == 1 => self.flow_rows().len(),
            Screen::Markets => self.market_rows().len(),
            Screen::Swap | Screen::Convert | Screen::Wrap => 0,
        }
    }

    /// Merged, newest-first activity rows: (time, is_operation, index).
    pub fn activity_rows(&self) -> Vec<(u64, bool, usize)> {
        let filter = if self.screen == Screen::Activity { self.activity_filter } else { ActivityFilter::All };
        let nft_kind = |k: &str| k.starts_with("nft");
        let trade_kind =
            |k: &str| k == "swap" || k.starts_with("curve") || k.starts_with("convert") || k.contains("wrap") || k.contains("claim");
        let mut rows: Vec<(u64, bool, usize)> = self
            .dash
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| o.status != OpStatus::Cancelled)
            .filter(|(_, o)| match filter {
                ActivityFilter::All => true,
                ActivityFilter::Sends => o.kind.starts_with("send") || o.kind == "nft_transfer",
                ActivityFilter::Receipts => false,
                ActivityFilter::Trades => trade_kind(&o.kind),
                ActivityFilter::Nfts => nft_kind(&o.kind),
            })
            .map(|(i, o)| (o.created, true, i))
            .collect();
        rows.extend(
            self.dash
                .activity
                .iter()
                .enumerate()
                .filter(|(_, a)| {
                    let nft = a.detail["standard"].as_str().is_some_and(|s| s != "ERC-20");
                    match filter {
                        ActivityFilter::All => true,
                        ActivityFilter::Sends => a.direction == "out",
                        ActivityFilter::Receipts => a.direction == "in",
                        ActivityFilter::Trades => false,
                        ActivityFilter::Nfts => nft,
                    }
                })
                .map(|(i, a)| (a.observed, false, i)),
        );
        rows.sort_by(|a, b| b.0.cmp(&a.0));
        rows
    }

    /// Stable key for an activity row (for the detail stack).
    pub fn activity_key(&self, index: usize) -> Option<String> {
        match self.activity_rows().get(index) {
            Some((_, true, i)) => self.dash.ops.get(*i).map(|o| format!("op:{}", o.id)),
            Some((_, false, i)) => self.dash.activity.get(*i).map(|a| format!("act:{}", a.key)),
            None => None,
        }
    }

    fn account_choices(&self) -> Vec<(String, String)> {
        self.dash
            .accounts
            .iter()
            .map(|a| {
                (
                    a.address.clone(),
                    format!(
                        "{} · {} QUAI",
                        a.label,
                        wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(a.balance, 18, 4))
                    ),
                )
            })
            .collect()
    }

    pub(crate) fn open_form(&mut self, kind: FormKind) {
        let accounts = self.account_choices();
        let preferred = self.dash.accounts.get(if self.screen == Screen::Accounts { self.selected } else { 0 }).map(|a| a.address.clone());
        let account = |label: &str| {
            let f = Field::new(label, "←/→ to choose").with(preferred.clone().unwrap_or_default());
            if accounts.is_empty() { Field::new(label, "label, address or #").optional() } else { f.choice(accounts.clone()) }
        };
        let directions = vec![("quai_to_qi".to_string(), "QUAI → Qi".to_string()), ("qi_to_quai".to_string(), "Qi → QUAI".to_string())];
        // A title only this arm can build (it names the contract); every other arm is a literal.
        let mut built_title: Option<String> = None;
        let (title, fields, note): (&str, Vec<Field>, Option<&str>) = match &kind {
            FormKind::BoundedSwap { input, .. } => (
                "Swap with explicit limits",
                vec![
                    account("Account"),
                    Field::new("Input amount", "pay token units").with(input.clone()),
                    Field::new("Minimum receive", "receive token units · optional").optional(),
                    Field::new("Maximum impact (bps)", "0 through 10000 · optional").optional(),
                    Field::new("Maximum fee", "QUAI · optional").optional(),
                ],
                Some("At least one explicit limit is required. Fresh route and allowance checks preserve it before each review."),
            ),
            FormKind::StakePosition { name, amount, stake, .. } => {
                built_title = Some(format!("{} {name} LP", if *stake { "Stake" } else { "Unstake" }));
                (
                    "LP position",
                    vec![account("Account"), Field::new("LP amount", "exact partial amount; choose signer first").with(amount.clone())],
                    Some("The selected account's live LP or staked balance bounds execution. Each transaction is reviewed."),
                )
            }
            FormKind::OrderCreate { .. } => (
                "Create limit order",
                super::order_ui::fields(account("Account")),
                Some("Fixed input from Swap. Creation signs nothing; each approval or swap requires a fresh review."),
            ),
            FormKind::ExactOutput { from, to } => (
                "Exact-output swap",
                vec![
                    account("Account"),
                    Field::new(&format!("Receive exactly ({to})"), "output amount"),
                    Field::new(&format!("Maximum input ({from})"), "strict spending limit"),
                    Field::new("Maximum fee", "QUAI · optional").optional(),
                ],
                Some("Every approval and swap is reviewed. The output is exact; unused input stays with you or is refunded."),
            ),
            FormKind::SendQuai => (
                "Send QUAI",
                vec![
                    account("From"),
                    Field::new("To", "address or contact name"),
                    Field::new("Amount", "").amount("QUAI"),
                    Field::new("Max fee", "QUAI · leave empty for the network default").optional(),
                ],
                None,
            ),
            FormKind::SendQi => (
                "Send Qi",
                vec![
                    Field::new("To", "payment code, contact or single-output Qi address"),
                    Field::new("Amount", "").amount("QI"),
                    Field::new("Max fee", "Qi · leave empty for the estimate").optional(),
                ],
                Some("Payment codes derive a fresh address for every output."),
            ),
            FormKind::SendToken => (
                "Send token",
                vec![
                    account("From"),
                    Field::new("Token", "symbol or contract"),
                    Field::new("To", "address or contact name"),
                    Field::new("Amount", "token units").amount("TOKEN"),
                ],
                None,
            ),
            FormKind::Approve => (
                "Approve spender",
                vec![
                    Field::new("Token", "symbol or contract"),
                    Field::new("Spender", "contract address"),
                    Field::new("Amount", "exact cap · type `unlimited` for no cap · 0 revokes"),
                ],
                Some("Approvals let the spender move your tokens up to the cap."),
            ),
            FormKind::ConvertQuaiToQi => (
                "Convert QUAI → Qi",
                vec![
                    account("From"),
                    Field::new("Amount", "minimum 10 QUAI").amount("QUAI"),
                    Field::new("Slippage", "empty: automatic from fresh quote; or manual basis points").optional(),
                ],
                Some("Conversions in one prime block share a discount; beyond your slippage it refunds (fee lost). Qi output time-locks."),
            ),
            FormKind::ConvertQiToQuai => (
                "Convert Qi → QUAI",
                vec![
                    account("To"),
                    Field::new("Amount", "").amount("QI"),
                    Field::new("Slippage", "empty: automatic from fresh quote; or manual basis points").optional(),
                ],
                Some("Converted QUAI is locked for the protocol lock period."),
            ),
            FormKind::RemoveLiquidity { name, .. } => (
                "Remove liquidity",
                vec![
                    account("Account"),
                    Field::new(&format!("Percent of your {name} position"), "1 to 100").with("100"),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some("Staked LP must be unstaked first — the router can only burn LP held in the account."),
            ),
            FormKind::Incentivize { name, .. } => (
                "Fund pool rewards",
                vec![
                    account("Account"),
                    Field::new("Reward token", "WQUAI, WQI or USDT").with("WQUAI"),
                    Field::new(&format!("Amount to give to {name} stakers"), "").amount("WQUAI"),
                    Field::new("Streamed over (days)", "1 to 365").with("30"),
                ],
                Some(
                    "This gives the tokens away: they go to whoever stakes LP in this pool. The gauge has no recover function, so it cannot be undone.",
                ),
            ),
            FormKind::CurveBuy { symbol, .. } => (
                "Buy on the bonding curve",
                vec![
                    account("Account"),
                    Field::new(&format!("QUAI to spend on {symbol}"), "").amount("QUAI"),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some("The curve quotes it before the review. A new token's price is set by its curve alone."),
            ),
            FormKind::CurveSell { symbol, held, .. } => (
                "Sell to the bonding curve",
                vec![
                    account("Account"),
                    Field::new(&format!("{symbol} to sell"), "whole tokens · prefilled with what you hold").with(held.clone()),
                    Field::new("Slippage", "basis points (100 = 1%)").with(self.config.swap_slippage_bps.to_string()),
                ],
                Some(
                    "Two steps: an exact approval for the curve, then the sale. Quainance credits QUAI for a later claim; Hartii pays QUAI directly.",
                ),
            ),
            FormKind::WrapQi => (
                "Wrap Qi → WQI (step 1 of 2)",
                vec![account("Beneficiary"), Field::new("Amount", "").amount("QI")],
                Some("After settlement, claim WQI (m)."),
            ),
            FormKind::UnwrapWqi => ("Unwrap WQI → Qi", vec![account("Account"), Field::new("Amount", "whole Qi").amount("WQI")], None),
            FormKind::WrapQuai => ("Wrap QUAI → WQUAI", vec![account("Account"), Field::new("Amount", "").amount("QUAI")], None),
            FormKind::UnwrapQuai => ("Unwrap WQUAI → QUAI", vec![account("Account"), Field::new("Amount", "").amount("WQUAI")], None),
            FormKind::Notify => (
                "Notify payment peer",
                vec![account("Gas from"), Field::new("Peer", "payment code or contact")],
                Some("Public: links your payment code to the peer on-chain."),
            ),
            FormKind::BoardPost { channel } => (
                "Post a message",
                vec![account("Post from"), Field::new(&format!("Message to #{channel}"), "up to 1024 bytes")],
                Some("Public and permanent: anyone can read it, it cannot be taken back, and it is signed by this account."),
            ),
            FormKind::BoardDm { peer, name } => (
                "Send a sealed message",
                vec![
                    account("Send from"),
                    Field::new(
                        &format!("Message to {}", name.clone().unwrap_or_else(|| wallet_core::session::short_code(peer))),
                        "only you two can read it",
                    ),
                ],
                Some("Encrypted, but not hidden: your address, the time and the size are public, and it cannot be taken back."),
            ),
            FormKind::FollowChannel => (
                "Follow a channel",
                vec![Field::new("Channel", "a name, up to 32 bytes")],
                Some("A channel is just a name: following one only decides what this wallet shows."),
            ),
            FormKind::RenameWallet(id) => {
                let current = self.wallets.iter().find(|w| &w.id == id).map(|w| w.name.clone()).unwrap_or_default();
                ("Rename wallet", vec![Field::new("Name", "letters, digits, - and _").with(&current)], None)
            }
            FormKind::AddAccount => ("Add Quai account", vec![Field::new("Label", "e.g. Savings").optional()], None),
            FormKind::RenameAccount(_) => ("Rename account", vec![Field::new("Label", "")], None),
            FormKind::NewQiAddress => ("New Qi / mining address", vec![Field::new("Label", "e.g. mining rig").with("mining")], None),
            FormKind::Contact(original) => {
                let existing = original.as_ref().and_then(|n| self.dash.contacts.iter().find(|c| c.name == *n)).cloned();
                (
                    if original.is_some() { "Edit contact" } else { "Add contact" },
                    vec![
                        Field::new("Name", "how you'll pick them when sending")
                            .with(existing.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
                        Field::new("Address", "Quai or Qi address (0x…)")
                            .with(existing.as_ref().and_then(|c| c.address.clone()).unwrap_or_default())
                            .optional(),
                        Field::new("Payment code", "PM8T… for private Qi payments")
                            .with(existing.as_ref().and_then(|c| c.payment_code.clone()).unwrap_or_default())
                            .optional(),
                        Field::new("Note", "").with(existing.map(|c| c.note).unwrap_or_default()).optional(),
                    ],
                    Some("An address, a payment code, or both. A payment code also lets the wallet find their payments automatically."),
                )
            }
            FormKind::ContactFromPeer { code, address } => {
                let existing = self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).cloned();
                (
                    if existing.is_some() { "Update contact" } else { "Name this contact" },
                    vec![
                        Field::new("Name", "how you'll pick them when sending")
                            .with(existing.as_ref().map(|c| c.name.clone()).unwrap_or_default()),
                        Field::new("Address", "the account this message came from")
                            .with(address.clone().or_else(|| existing.as_ref().and_then(|c| c.address.clone())).unwrap_or_default())
                            .optional(),
                        Field::new("Payment code", "").with(code.clone()),
                        Field::new("Note", "").with(existing.map(|c| c.note).unwrap_or_default()).optional(),
                    ],
                    Some(
                        "The payment code is who they are; the address is one account they write from. Both are kept, and later accounts are added as they appear.",
                    ),
                )
            }
            FormKind::ImportToken => ("Import token", vec![Field::new("Contract address", "0x…")], None),
            FormKind::DeepScan => {
                ("Deep scan Qi", vec![Field::new("Scan to raw index", "").with("200000")], Some("Deep scans can take a while."))
            }
            FormKind::ExportPhrase => (
                "Reveal recovery phrase",
                vec![Field::new("Password", "re-enter your wallet password").secret()],
                Some("Make sure nobody can see your screen."),
            ),
            FormKind::Backup => (
                "Encrypted backup",
                vec![
                    Field::new("File", "").with(default_backup_path()),
                    Field::new("Backup password", "separate from the wallet password").new_secret(),
                ],
                None,
            ),
            FormKind::NftTransfer { contract, token_id, multi } => (
                "Transfer NFT",
                {
                    let mut f = vec![account("From"), Field::new("To", "Quai address or contact name")];
                    if *multi {
                        f.push(Field::new("Quantity", "whole number").with("1"));
                    }
                    let _ = (contract, token_id);
                    f
                },
                Some("Ownership is re-checked on-chain; NFT transfers cannot be undone."),
            ),
            FormKind::Monitor { network } => (
                "Monitoring endpoint",
                {
                    let current = self.config.monitor_endpoints.get(network).cloned();
                    let pathing = vec![
                        ("exact".to_string(), "exact Cyprus-1 URL".to_string()),
                        ("gateway".to_string(), "gateway base (derives /cyprus1)".to_string()),
                    ];
                    let mut mode = Field::new("URL type", "←/→");
                    if current.as_ref().is_some_and(|c| c.use_pathing) {
                        mode = mode.with("gateway");
                    }
                    vec![
                        Field::new("URL", "e.g. http://10.0.0.12:9200 · empty clears")
                            .with(current.map(|c| c.rpc_url).unwrap_or_default())
                            .optional(),
                        mode.choice(pathing),
                    ]
                },
                Some(
                    "Market data, charts and balance re-reads use it; reviews, signing and broadcasting always use the network's main RPC. The endpoint must report this network's chain id and genesis.",
                ),
            ),
            FormKind::DaemonUnlock => (
                "Unlock in the daemon",
                vec![Field::new("Password", "this wallet's password").secret()],
                Some(
                    "The daemon then holds this wallet unlocked after the terminal closes: it runs its interval conversions and reads its sealed chats. The password goes over the daemon's private socket, only after checking the other end is your daemon, and is not stored. quai-terminal daemon lock takes it back.",
                ),
            ),
            FormKind::Alert { name, .. } => (
                "Set an alert",
                vec![
                    Field::new("When", "←/→").choice(vec![
                        ("above".into(), format!("{name} rises to")),
                        ("below".into(), format!("{name} falls to")),
                        ("moves".into(), format!("{name} moves in 24h by (%)")),
                    ]),
                    Field::new("Value", "a price in the quote token, or a percentage"),
                ],
                Some(
                    "It fires once when the line is crossed, and again only after it has crossed back. The daemon checks while it runs; otherwise this window checks every minute while unlocked.",
                ),
            ),
            FormKind::ContractCall { address, name, functions } => {
                let choices: Vec<(String, String)> = functions.iter().map(|c| (c.signature.clone(), c.label())).collect();
                let first = functions.first().cloned();
                let mut fields = vec![account("From"), Field::new("Function", "←/→ to choose").choice(choices)];
                if let Some(c) = &first {
                    if c.payable {
                        fields.push(Field::new("QUAI to send", "this function accepts QUAI").optional().amount("QUAI"));
                    }
                    for (arg, ty) in &c.inputs {
                        let label = if arg.is_empty() { ty.clone() } else { format!("{arg} ({ty})") };
                        fields.push(Field::new(&label, &argument_hint(ty)));
                    }
                }
                built_title = Some(format!("Call {name} · {}", wallet_core::session::short_code(address)));
                (
                    "",
                    fields,
                    Some(
                        "The arguments are typed against the ABI this contract publishes about itself. That proves what was published, not what the deployed code does — the review shows the exact call data that gets signed.",
                    ),
                )
            }
            FormKind::IpfsGateway(content) => {
                use wallet_core::ipfs::Content;
                let current = match content {
                    Content::Abi => self.config.abi_ipfs_gateway.clone(),
                    Content::Media => self.config.ipfs_gateway.clone(),
                };
                let hint = format!("http://127.0.0.1:8080 · https://{{cid}}.ipfs.dweb.link · empty = {}", content.default_gateway());
                (
                    match content {
                        Content::Abi => "IPFS gateway for contract ABIs",
                        Content::Media => "IPFS gateway for images and NFT metadata",
                    },
                    vec![Field::new("URL", &hint).with(current.unwrap_or_default()).optional()],
                    Some(match content {
                        Content::Abi => {
                            "Where a contract's own metadata — the ABI its bytecode names by CID — is fetched from. ipfs.qu.ai is the authority for these: it is what Quai's deploy tooling pins to and what Quaiscan verifies against, so leaving this alone is right unless you run a node that pins Quai contract metadata itself. It is tested before it is saved."
                        }
                        Content::Media => {
                            "Where NFT images and metadata on IPFS are fetched from. This is the one worth pointing at your own node (Kubo's gateway, usually port 8080): it is the bulk of the fetching and the most revealing. A node on this machine or your network may be plain http and is reached directly, not through the proxy; a public gateway must be https. It is tested before it is saved: a file is fetched through it and checked against its CID."
                        }
                    }),
                )
            }
            FormKind::NftList { name, current, .. } => (
                if current.is_some() { "Change listing price" } else { "List for sale" },
                {
                    let currencies = ["QUAI", "WQI", "WQUAI", "USDT"].iter().map(|c| (c.to_string(), c.to_string())).collect();
                    let (price, currency) = current.clone().unwrap_or_default();
                    let mut currency_field = Field::new("Currency", "←/→");
                    if !currency.is_empty() {
                        currency_field = currency_field.with(currency);
                    }
                    let _ = name;
                    vec![Field::new("Price", "e.g. 250").with(price), currency_field.choice(currencies)]
                },
                Some(
                    "Bazarr shows it within a minute. Anyone can buy at this price until you cancel (X). Approvals, if needed, come first as their own reviews.",
                ),
            ),
            FormKind::Quote => (
                "Conversion quote",
                vec![Field::new("Direction", "←/→").choice(directions), Field::new("Amount", "source asset")],
                Some("Shows the node quote and batch-discount scenarios; nothing is signed."),
            ),
        };
        let focus = fields.iter().position(|f| f.value.is_empty() && !matches!(f.kind, FieldKind::Choice(_))).unwrap_or(0);
        self.contract_probe = None;
        self.contract_found = None;
        self.contract_asked.clear();
        self.modal = Modal::Form(Form {
            kind,
            title: built_title.unwrap_or_else(|| title.into()),
            fields,
            focus,
            note: note.map(str::to_string),
            contract_note: None,
            pending: false,
            error: None,
            error_field: None,
        });
        self.dirty = true;
    }

    fn submit_form(&mut self, form: &Form) {
        let v = |i: usize| form.fields.get(i).map(|f| f.value.trim().to_string()).unwrap_or_default();
        let opt = |i: usize| Some(v(i)).filter(|s| !s.is_empty());
        let bps = |i: usize| v(i).parse::<u16>().unwrap_or(self.config.swap_slippage_bps);
        if matches!(form.kind, FormKind::ConvertQuaiToQi | FormKind::ConvertQiToQuai) {
            let direction = if form.kind == FormKind::ConvertQiToQuai {
                wallet_core::qi_market::Direction::QiToQuai
            } else {
                wallet_core::qi_market::Direction::QuaiToQi
            };
            self.start_protocol_conversion(direction, v(1), opt(2).and_then(|v| v.parse().ok()), opt(0));
            return;
        }
        if let FormKind::Monitor { network } = &form.kind {
            self.set_monitor(network, &v(0), v(1) == "gateway");
            return;
        }
        if let FormKind::IpfsGateway(content) = &form.kind {
            self.set_ipfs_gateway(*content, &v(0));
            return;
        }
        if let FormKind::DaemonUnlock = &form.kind {
            if let Some(id) = self.meta.as_ref().map(|m| m.id.clone()) {
                // The form's own buffer is wiped when the form drops; this copy, when the task ends.
                let password = Zeroizing::new(form.fields[0].value.clone());
                self.hand_to_daemon(id, password);
            }
            return;
        }
        if let FormKind::Alert { pool, name, inverted } = &form.kind {
            let Ok(value) = v(1).parse::<f64>() else {
                self.toast("the value is a number, like 125 or 10", true);
                return;
            };
            if !(value.is_finite() && value > 0.0) {
                self.toast("the value must be above zero", true);
                return;
            }
            use wallet_core::alerts::{Alert, Rule};
            let rule = match v(0).as_str() {
                "below" => Rule::Below { price: value },
                "moves" => Rule::Moves { pct: value },
                _ => Rule::Above { price: value },
            };
            let alert = Alert { id: 0, pool: pool.clone(), name: name.clone(), inverted: *inverted, rule, active: false, fired: 0 };
            self.send_data(super::data::DataCmd::Alerts(super::data::AlertOp::Add(Box::new(alert))));
            return;
        }
        if let FormKind::NftList { contract, token_id, owner, name, current } = &form.kind {
            let price = v(0);
            let currency = v(1);
            let label = if current.is_some() {
                format!("re-price {name} to {price} {currency}")
            } else {
                format!("list {name} for {price} {currency}")
            };
            self.start_flow(super::eco::FlowKind::NftList {
                account: Some(owner.clone()),
                contract: contract.clone(),
                token_id: token_id.clone(),
                price: Some(price),
                currency,
                label,
            });
            return;
        }
        if let FormKind::RenameWallet(id) = &form.kind {
            let name = v(0);
            let Some(mut meta) = self.wallets.iter().find(|w| &w.id == id).cloned() else {
                self.toast("that wallet is gone", true);
                return;
            };
            match self.registry.rename(&mut meta, &name) {
                Ok(()) => {
                    // The open wallet keeps its new name in the header and as the default.
                    if self.meta.as_ref().is_some_and(|m| m.id == meta.id) {
                        self.config.default_wallet = Some(meta.name.clone());
                        self.save_config();
                        self.meta = Some(meta.clone());
                        self.dash.meta = Some(meta.clone());
                    }
                    self.load_wallets();
                    self.toast(format!("renamed to `{}`", meta.name), false);
                }
                Err(e) => self.toast(friendly_error(&e.to_string()), true),
            }
            return;
        }
        if let FormKind::FollowChannel = &form.kind {
            self.follow_channel(&v(0));
            return;
        }
        // Sequences: the worker answers each of these with the next review it needs — an exact
        // approval while one is outstanding, then the operation. Sent as a bare command they
        // would stop after the approval, so they go through the flow driver.
        let steps = match &form.kind {
            FormKind::BoundedSwap { from, to, .. } => Some((
                Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent {
                        account: v(0),
                        max_fee: opt(4),
                        action: wallet_core::execution::TradingAction::BoundedSwap {
                            from: from.clone(),
                            to: to.clone(),
                            amount: v(1),
                            slippage: self.config.swap_slippage_bps,
                            deadline: self.config.swap_deadline_minutes,
                            bounds: wallet_core::swap::SwapBounds {
                                minimum_output: opt(2),
                                maximum_impact_bps: opt(3).and_then(|v| v.parse().ok()),
                            },
                        },
                    },
                },
                "bounded swap".into(),
            )),
            FormKind::StakePosition { pair, gauge, name, stake, .. } => Some((
                if *stake {
                    Prepare::StakeNext { account: opt(0), pair: pair.clone(), gauge: gauge.clone(), amount: v(1) }
                } else {
                    Prepare::Unstake { account: opt(0), pair: pair.clone(), gauge: gauge.clone(), amount: v(1) }
                },
                format!("{} {name} LP", if *stake { "stake" } else { "unstake" }),
            )),
            FormKind::ExactOutput { from, to } => Some((
                Prepare::Trading {
                    intent: wallet_core::execution::TradingIntent {
                        account: v(0),
                        max_fee: opt(3),
                        action: wallet_core::execution::TradingAction::ExactOutput {
                            from: from.clone(),
                            to: to.clone(),
                            output: v(1),
                            max_input: v(2),
                            deadline: self.config.swap_deadline_minutes,
                        },
                    },
                },
                format!("exact output {from} → {to}"),
            )),
            FormKind::RemoveLiquidity { pair, name } => Some((
                Prepare::RemoveLiquidityNext {
                    account: opt(0),
                    pair: pair.clone(),
                    percent: v(1).parse().unwrap_or(100),
                    slippage: bps(2),
                    deadline: self.config.swap_deadline_minutes,
                },
                format!("remove liquidity from {name}"),
            )),
            FormKind::CurveSell { token, symbol, curve, .. } => Some((
                Prepare::CurveSellNext {
                    account: opt(0),
                    token: token.clone(),
                    symbol: symbol.clone(),
                    curve: curve.clone(),
                    amount: v(1),
                    slippage: bps(2),
                    deadline: Some(self.config.swap_deadline_minutes),
                },
                format!("sell {symbol} to its curve"),
            )),
            FormKind::Incentivize { pair, name } => Some((
                Prepare::IncentivizeNext {
                    account: opt(0),
                    pair: pair.clone(),
                    token: v(1),
                    amount: v(2),
                    days: v(3).parse().unwrap_or(30),
                },
                format!("fund {name} rewards"),
            )),
            _ => None,
        };
        if let Some((prepare, label)) = steps {
            self.start_flow(super::eco::FlowKind::Steps { prepare: Box::new(prepare), label });
            return;
        }
        let cmd = match &form.kind {
            FormKind::SendQuai => Cmd::Prepare(Prepare::SendQuai { from: opt(0), to: v(1), amount: v(2), max_fee: opt(3) }),
            FormKind::SendQi => Cmd::Prepare(Prepare::SendQi { to: v(0), amount: v(1), max_fee: opt(2) }),
            FormKind::SendToken => Cmd::Prepare(Prepare::SendToken { from: opt(0), token: v(1), to: v(2), amount: v(3) }),
            FormKind::Approve => Cmd::Prepare(Prepare::Approve {
                token: v(0),
                spender: v(1),
                amount: Some(v(2)).filter(|a| !a.eq_ignore_ascii_case("unlimited")),
            }),
            FormKind::ConvertQuaiToQi | FormKind::ConvertQiToQuai => unreachable!("handled above as core plans"),
            FormKind::WrapQi => Cmd::Prepare(Prepare::WrapQi { account: opt(0), amount: v(1) }),
            FormKind::UnwrapWqi => Cmd::Prepare(Prepare::UnwrapWqi { account: opt(0), amount: v(1) }),
            FormKind::WrapQuai => Cmd::Prepare(Prepare::WrapQuai { account: opt(0), amount: v(1) }),
            FormKind::UnwrapQuai => Cmd::Prepare(Prepare::UnwrapQuai { account: opt(0), amount: v(1) }),
            FormKind::Notify => Cmd::Prepare(Prepare::Notify { from: opt(0), peer: v(1) }),
            FormKind::BoardPost { channel } => Cmd::Prepare(Prepare::BoardPost { from: opt(0), channel: channel.clone(), text: v(1) }),
            FormKind::BoardDm { peer, .. } => Cmd::Prepare(Prepare::BoardDm { from: opt(0), peer: peer.clone(), text: v(1) }),
            FormKind::OrderCreate { .. } | FormKind::FollowChannel | FormKind::RenameWallet(_) => unreachable!("handled above"),
            FormKind::AddAccount => Cmd::AddAccount(opt(0)),
            FormKind::RenameAccount(a) => Cmd::RenameAccount { account: a.clone(), label: v(0) },
            FormKind::NewQiAddress => Cmd::NewQiAddress(opt(0)),
            FormKind::Contact(original) => {
                Cmd::SaveContact { original: original.clone(), name: v(0), address: opt(1), code: opt(2), note: v(3) }
            }
            FormKind::ContactFromPeer { code, .. } => {
                // Editing the person already behind this code, when there is one: the code is
                // the identity, so this must not create a second contact holding it.
                let original = self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).map(|c| c.name.clone());
                Cmd::SaveContact { original, name: v(0), address: opt(1), code: opt(2), note: v(3) }
            }
            FormKind::ImportToken => Cmd::ImportToken(v(0)),
            FormKind::DeepScan => Cmd::ScanQi { deep: v(0).parse().ok() },
            FormKind::ExportPhrase => Cmd::ExportPhrase(Zeroizing::new(v(0))),
            FormKind::Backup => Cmd::Backup { path: v(0), password: Zeroizing::new(v(1)) },
            FormKind::Quote => Cmd::Quote { direction: v(0), amount: v(1) },
            FormKind::ContractCall { address, functions, .. } => {
                let signature = v(1);
                let Some(callable) = functions.iter().find(|c| c.signature == signature) else { return };
                let mut rest = form.fields.iter().skip(2);
                let value = callable.payable.then(|| rest.next().map(|f| f.value.trim().to_string())).flatten();
                let args: Vec<String> = rest.map(|f| f.value.trim().to_string()).collect();
                Cmd::Prepare(Prepare::ContractCall {
                    account: opt(0),
                    address: address.clone(),
                    signature,
                    args,
                    value: value.filter(|v| !v.is_empty()),
                })
            }
            FormKind::NftList { .. }
            | FormKind::Monitor { .. }
            | FormKind::IpfsGateway(_)
            | FormKind::Alert { .. }
            | FormKind::DaemonUnlock => {
                return;
            }
            FormKind::BoundedSwap { .. }
            | FormKind::StakePosition { .. }
            | FormKind::ExactOutput { .. }
            | FormKind::RemoveLiquidity { .. }
            | FormKind::Incentivize { .. }
            | FormKind::CurveSell { .. } => {
                unreachable!("handled above as sequences")
            }
            FormKind::CurveBuy { token, symbol, curve } => Cmd::Prepare(Prepare::CurveBuy {
                account: opt(0),
                token: token.clone(),
                symbol: symbol.clone(),
                curve: curve.clone(),
                amount: v(1),
                slippage: bps(2),
                deadline: Some(self.config.swap_deadline_minutes),
            }),
            FormKind::NftTransfer { contract, token_id, multi } => Cmd::Prepare(Prepare::NftTransfer {
                account: opt(0),
                contract: contract.clone(),
                token_id: token_id.clone(),
                to: v(1),
                quantity: if *multi { opt(2) } else { None },
            }),
        };
        self.send(cmd);
    }

    pub fn open_palette(&mut self) {
        self.load_palette_recent();
        self.modal = Modal::Palette { query: String::new(), selected: 0 };
    }

    pub fn run_action(&mut self, id: &str) {
        if let Some(feature) = action_feature(id).filter(|f| !self.config.features.on(*f)) {
            self.toast(format!("{} · System › Settings", feature.off_note()), false);
            return;
        }
        let needs_keys = matches!(
            id,
            "send_quai"
                | "send_qi"
                | "send_token"
                | "convert_quai_qi"
                | "convert_qi_quai"
                | "wrap_qi"
                | "claim_wqi"
                | "unwrap_wqi"
                | "wrap_quai"
                | "unwrap_quai"
                | "approve"
                | "aggregate"
                | "discover"
                | "add_peer"
                | "notify"
                | "speedup"
                | "export_phrase"
                | "fill_gap"
        );
        if needs_keys && !self.can_sign() {
            self.toast("this wallet is watch-only", true);
            return;
        }
        match id {
            "send_quai" => self.open_form(FormKind::SendQuai),
            "send_qi" => self.open_form(FormKind::SendQi),
            "send_token" => self.open_form(FormKind::SendToken),
            "receive_quai" => self.modal = Modal::Receive { asset_qi: false, account: 0 },
            "receive_qi" => self.modal = Modal::Receive { asset_qi: true, account: 0 },
            "convert_quai_qi" => self.open_form(FormKind::ConvertQuaiToQi),
            "convert_qi_quai" => self.open_form(FormKind::ConvertQiToQuai),
            "quote" => self.open_form(FormKind::Quote),
            "wrap_qi" => self.open_form(FormKind::WrapQi),
            "claim_wqi" => self.claim_now(None),
            "unwrap_wqi" => self.open_form(FormKind::UnwrapWqi),
            "wrap_quai" => self.open_form(FormKind::WrapQuai),
            "unwrap_quai" => self.open_form(FormKind::UnwrapQuai),
            "approve" => self.open_form(FormKind::Approve),
            "import_token" => self.open_form(FormKind::ImportToken),
            "add_account" => self.open_form(FormKind::AddAccount),
            "new_qi_address" => self.open_form(FormKind::NewQiAddress),
            "scan_qi" => self.send(Cmd::ScanQi { deep: None }),
            "deep_scan" => self.open_form(FormKind::DeepScan),
            "aggregate" => self.send(Cmd::Prepare(Prepare::Consolidate { aggregate: true })),
            "sweep" => self.send(Cmd::Prepare(Prepare::Consolidate { aggregate: false })),
            "discover" => self.send(Cmd::DiscoverMailbox),
            "add_peer" => {
                self.open_form(FormKind::Contact(None));
                if let Modal::Form(f) = &mut self.modal {
                    f.focus = 2;
                }
            }
            "notify" => self.open_form(FormKind::Notify),
            "add_contact" => self.open_form(FormKind::Contact(None)),
            "contacts" => self.switch(Screen::Contacts),
            "trade" => self.open_trade(),
            "swap" => self.switch(Screen::Swap),
            "portfolio" => self.switch(Screen::Home),
            "home" => self.switch(Screen::Home),
            "nfts" => self.switch(Screen::Collected),
            "explore" => self.switch(Screen::Explore),
            "listings" => self.switch(Screen::Listings),
            "data_sources" => self.switch(Screen::DataSources),
            "launches" => self.switch(Screen::Launches),
            "locks" => self.switch(Screen::Locks),
            "discover_tokens" => self.send(Cmd::DiscoverTokens),
            "test_data" => self.send_data(super::data::DataCmd::Test),
            "speedup" => {
                let rows = self.activity_rows();
                match rows.get(self.selected) {
                    Some((_, true, i)) if self.screen == Screen::Activity && self.dash.ops[*i].status.replaceable() => {
                        let id = self.dash.ops[*i].id.clone();
                        self.send(Cmd::Prepare(Prepare::SpeedUp { op: id }));
                    }
                    Some((_, true, i)) if self.screen == Screen::Activity && !self.dash.ops[*i].status.is_terminal() => {
                        self.toast("this transaction is already mined; nothing to speed up", true)
                    }
                    _ => self.toast("select a pending transaction on the activity screen first", true),
                }
            }
            "fill_gap" => self.send(Cmd::Prepare(Prepare::FillGap { from: None })),
            "themes" => self.modal = Modal::Themes(Picker::new(self)),
            "glossary" => self.modal = Modal::Glossary { selected: 0 },
            "daemon_unlock" => match crate::daemon::state(&self.paths) {
                _ if !self.can_sign() => self.toast("this wallet is watch-only: the daemon watches it without keys", true),
                None => self.toast("the daemon is not running · quai-terminal daemon start", true),
                Some(d) if self.meta.as_ref().is_some_and(|m| d.unlocked(&m.id)) => {
                    self.toast("the daemon already holds this wallet unlocked", false)
                }
                Some(_) => self.open_form(FormKind::DaemonUnlock),
            },
            "lock_gallery" => self.modal = Modal::Effects(Gallery::new(&self.config.lock_effect)),
            "refresh" => self.send(Cmd::Refresh { full: true }),
            "lock" => self.lock_now(None),
            "export_phrase" => self.open_form(FormKind::ExportPhrase),
            "backup" => self.open_form(FormKind::Backup),
            "network" => self.switch(Screen::Network),
            "notifications" => {
                self.modal = Modal::Notifications;
                self.send(Cmd::MarkRead);
            }
            "matrix" => {
                if self.motion() == Motion::Off {
                    self.toast("motion is off (Settings → Motion)", false);
                } else {
                    let args = super::fx::theme_args("matrix", &self.theme);
                    self.ambient = Ceremony::with_args("matrix", &args, "follow the white rabbit", 100, 30, 420)
                        .map(|c| c.at_speed(super::fx::LOCK_SPEED));
                }
            }
            "poem" => {
                if self.motion() == Motion::Off {
                    self.toast("motion is off (Settings → Motion)", false);
                } else {
                    match super::fx::poem_rain(self.recent_hashes.iter().map(String::as_str)) {
                        Some(text) => {
                            let args = super::fx::theme_args("rain", &self.theme);
                            self.ambient =
                                Ceremony::with_args("rain", &args, &text, 100, 30, 360).map(|c| c.at_speed(super::fx::LOCK_SPEED));
                            self.poem_haiku = Some(super::fx::POEM_HAIKU.to_string());
                        }
                        None => self.toast("waiting for a few blocks to fall", false),
                    }
                }
            }
            "help" => self.modal = Modal::Help,
            "quit" => self.quit = true,
            _ => {}
        }
        self.dirty = true;
    }

    pub fn can_sign(&self) -> bool {
        self.meta.as_ref().is_some_and(|m| m.kind != WalletKind::Watch)
    }

    /// Seconds until auto-lock, when it applies.
    pub fn autolock_remaining(&self) -> Option<u64> {
        (!self.locked && self.can_sign() && self.config.auto_lock_minutes > 0)
            .then(|| (u64::from(self.config.auto_lock_minutes) * 60).saturating_sub(self.last_input.elapsed().as_secs()))
    }

    pub fn on_key(&mut self, key: KeyEvent, size: (u16, u16)) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        self.last_input = Instant::now();
        self.dirty = true;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if matches!(self.modal, Modal::Review(_) | Modal::Form(_)) {
                self.modal = Modal::Confirm {
                    title: "Quit".into(),
                    body: "Quit with a form or review open? Nothing is signed.".into(),
                    action: ConfirmAction::Quit,
                };
            } else {
                self.quit = true;
            }
            return;
        }
        self.lock_warned = false;
        // A key skips a decorative effect (never while typing into a modal; celebrations just fade).
        if !self.locked && matches!(self.modal, Modal::None) && (self.ceremony.is_some() || self.ambient.is_some()) {
            self.ceremony = None;
            self.ambient = None;
            return;
        }
        if self.onboarding.is_some() {
            return; // handled in the onboarding module
        }
        if self.locked && matches!(self.modal, Modal::None) {
            match key.code {
                // Typing during an unlock would land in the next attempt's password, so the
                // keyboard is ignored until this one answers.
                _ if self.unlocking => {}
                KeyCode::Enter if !self.lock_input.is_empty() => {
                    let password = Zeroizing::new(std::mem::take(&mut self.lock_input));
                    self.begin_unlock(password);
                }
                KeyCode::Backspace => {
                    self.lock_input.pop();
                }
                KeyCode::Esc => self.lock_input.zeroize(),
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.lock_input.push(c);
                    self.lock_error = None;
                }
                _ => {}
            }
            return;
        }
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        self.modal = match modal {
            Modal::None => {
                self.on_screen_key(key, size);
                return;
            }
            Modal::Form(form) => self.form_key(form, key),
            Modal::Review(mut r) => match key.code {
                // The same send as a shell command; copying signs nothing and keeps the review open.
                KeyCode::Char('y') => {
                    match review_cli(&r.review) {
                        Some(cli) => {
                            self.clipboard = Some(cli);
                            self.toast("command copied · the review is still open", false);
                        }
                        None => self.toast("only sends have a command-line form yet", true),
                    }
                    Modal::Review(r)
                }
                KeyCode::Esc => {
                    self.send(Cmd::Discard(r.review.op_id.clone()));
                    self.flow_on_rejected(&r.review.op_id);
                    Modal::None
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                    r.approve_focused = !r.approve_focused;
                    Modal::Review(r)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    r.scroll = (r.scroll + 1).min(r.content_lines.saturating_sub(r.viewport));
                    Modal::Review(r)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    r.scroll = r.scroll.saturating_sub(1);
                    Modal::Review(r)
                }
                KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('G') | KeyCode::End => {
                    let jump = if matches!(key.code, KeyCode::Char('G') | KeyCode::End) { r.content_lines } else { r.viewport };
                    r.scroll = (r.scroll + jump).min(r.content_lines.saturating_sub(r.viewport));
                    Modal::Review(r)
                }
                KeyCode::PageUp => {
                    r.scroll = r.scroll.saturating_sub(r.viewport);
                    Modal::Review(r)
                }
                KeyCode::Enter => {
                    if r.approve_focused && r.can_approve() {
                        self.committing_kind = Some(r.review.kind.clone());
                        self.ceremony = None;
                        self.send(Cmd::Commit(r.review.op_id.clone()));
                        Modal::None
                    } else if r.approve_focused {
                        self.toast("read to the end of the review first (space pages down)", true);
                        Modal::Review(r)
                    } else {
                        self.send(Cmd::Discard(r.review.op_id.clone()));
                        self.flow_on_rejected(&r.review.op_id);
                        Modal::None
                    }
                }
                _ => Modal::Review(r),
            },
            Modal::Palette { mut query, mut selected } => {
                let matches = self.palette_entries(&query);
                // ctrl-y: take the selected entry's shell command, for a script.
                if key.code == KeyCode::Char('y') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    match matches.get(selected).map(|e| e.cli.clone()).filter(|c| !c.is_empty()) {
                        Some(cli) => {
                            self.clipboard = Some(cli);
                            self.toast("command copied", false);
                        }
                        None => self.toast("this one has no command-line form", true),
                    }
                    self.modal = Modal::Palette { query, selected };
                    return;
                }
                match key.code {
                    KeyCode::Esc => Modal::None,
                    KeyCode::Down | KeyCode::Tab => {
                        selected = (selected + 1).min(matches.len().saturating_sub(1));
                        Modal::Palette { query, selected }
                    }
                    KeyCode::Up | KeyCode::BackTab => {
                        selected = selected.saturating_sub(1);
                        Modal::Palette { query, selected }
                    }
                    KeyCode::Enter => {
                        if let Some(e) = matches.into_iter().nth(selected) {
                            self.run_palette(e);
                            return;
                        }
                        Modal::None
                    }
                    KeyCode::Backspace => {
                        query.pop();
                        Modal::Palette { query, selected: 0 }
                    }
                    KeyCode::Char(c) => {
                        query.push(c);
                        Modal::Palette { query, selected: 0 }
                    }
                    _ => Modal::Palette { query, selected },
                }
            }
            Modal::Receive { asset_qi, account } => {
                self.kitty.clear(self.caps.tmux);
                match key.code {
                    KeyCode::Tab | KeyCode::Left | KeyCode::Right => Modal::Receive { asset_qi: !asset_qi, account },
                    KeyCode::Down | KeyCode::Char('j') => {
                        Modal::Receive { asset_qi, account: (account + 1) % self.dash.accounts.len().max(1) }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        Modal::Receive { asset_qi, account: account.checked_sub(1).unwrap_or(self.dash.accounts.len().saturating_sub(1)) }
                    }
                    KeyCode::Char('y') => {
                        self.clipboard = self.receive_value(asset_qi, account);
                        if self.clipboard.is_some() {
                            self.toast("copied to clipboard", false);
                        }
                        Modal::Receive { asset_qi, account }
                    }
                    KeyCode::Char('n') if asset_qi => {
                        self.send(Cmd::NewQiAddress(Some("receive".into())));
                        Modal::Receive { asset_qi, account }
                    }
                    _ => Modal::None,
                }
            }
            // Any key hides the phrase; it is zeroized on drop.
            Modal::Secret { .. } => Modal::None,
            // Results carry a tx hash worth reading: only deliberate keys close them.
            m @ (Modal::Result(_) | Modal::Quote(_)) => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => Modal::None,
                KeyCode::Char('c') if matches!(m, Modal::Quote(_)) => {
                    self.modal = Modal::None;
                    self.run_action("convert_quai_qi");
                    return;
                }
                KeyCode::Char('C') if matches!(m, Modal::Quote(_)) => {
                    self.modal = Modal::None;
                    self.run_action("convert_qi_quai");
                    return;
                }
                _ => m,
            },
            Modal::Confirm { title, body, action } => match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    match action {
                        ConfirmAction::Quit => self.quit = true,
                        ConfirmAction::RemoveContact(name) => self.send(Cmd::RemoveContact(name)),
                        ConfirmAction::SwitchNetwork(id) => self.switch_network(id),
                        ConfirmAction::AcceptOffer(code) => self.send(Cmd::AcceptOffer(code)),
                        ConfirmAction::DeclineOffer(code) => self.send(Cmd::DeclineOffer(code)),
                    }
                    Modal::None
                }
                KeyCode::Char('q') if matches!(action, ConfirmAction::Quit) => {
                    self.quit = true;
                    Modal::None
                }
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Enter => Modal::None,
                _ => Modal::Confirm { title, body, action },
            },
            Modal::Themes(mut picker) => match picker.on_key(key, &mut self.theme) {
                PickerOutcome::Open => Modal::Themes(picker),
                PickerOutcome::Cancelled => Modal::None,
                PickerOutcome::Applied => {
                    if let Some(e) = picker.current() {
                        self.config.theme = e.id.clone();
                        self.theme_override = None;
                        let name = e.name.clone();
                        self.save_config();
                        self.pending_theme_reload = true;
                        self.toast(format!("theme · {name}"), false);
                    }
                    Modal::None
                }
            },
            Modal::Effects(mut g) => {
                let n = super::fx::EFFECTS.len() + 1;
                match key.code {
                    KeyCode::Esc => Modal::None,
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                        g.selected = (g.selected + 1) % n;
                        g.preview = None;
                        Modal::Effects(g)
                    }
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
                        g.selected = (g.selected + n - 1) % n;
                        g.preview = None;
                        Modal::Effects(g)
                    }
                    KeyCode::Enter => {
                        self.config.lock_effect = g.value().to_string();
                        self.save_config();
                        self.toast(format!("lock screen · {}", g.value()), false);
                        Modal::None
                    }
                    _ => Modal::Effects(g),
                }
            }
            Modal::Orders { rows, selected } => super::order_ui::key(self, rows, selected, key),
            Modal::TokenPicker { pay, query, selected } => self.picker_key(pay, query, selected, key),
            // From the key overlay, g opens the glossary; anything else closes it.
            Modal::Help if key.code == KeyCode::Char('g') => {
                self.help_moved = false;
                Modal::Glossary { selected: 0 }
            }
            Modal::Help | Modal::Notifications => {
                self.help_moved = false;
                Modal::None
            }
            Modal::Glossary { selected } => {
                let n = super::glossary::TERMS.len();
                match key.code {
                    KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => Modal::Glossary { selected: (selected + 1) % n },
                    KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => Modal::Glossary { selected: (selected + n - 1) % n },
                    KeyCode::Char('g') => Modal::Glossary { selected: 0 },
                    KeyCode::Char('G') => Modal::Glossary { selected: n - 1 },
                    _ => Modal::None,
                }
            }
        };
    }

    pub(crate) fn form_key(&mut self, mut form: Form, key: KeyEvent) -> Modal {
        if form.pending {
            // Esc abandons the wait; the worker result then arrives as a toast.
            return if key.code == KeyCode::Esc { Modal::None } else { Modal::Form(form) };
        }
        let n = form.fields.len();
        let is_choice = matches!(form.fields[form.focus].kind, FieldKind::Choice(_));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // The function this form is on before the key is handled. Only a key that actually changes
        // it may rebuild the argument fields — moving the cursor onto the picker must not, or a
        // Tab past it throws away everything typed into the arguments below.
        let chosen = |form: &Form| form.fields.iter().find(|f| f.label == "Function").map(|f| f.value.clone());
        let was_chosen = chosen(&form);
        // ^F on a destination that turned out to be a callable contract swaps this form for the
        // one that calls it, keeping the address.
        // ^F only means anything on a form that has a destination, and only once that destination
        // has come back a callable contract. Anywhere else — including inside the call form it
        // opens, where it would throw away typed arguments — it is left alone.
        if ctrl && key.code == KeyCode::Char('f') && Self::destination_field(&form.kind).is_some() {
            if let Some(found) = self.contract_found.clone().filter(|f| f.metadata.is_some())
                && self.open_contract_call(found)
            {
                return std::mem::replace(&mut self.modal, Modal::None);
            }
            // Nothing opened (no ABI, nothing callable in it): the form being typed into stays.
            return Modal::Form(form);
        }
        match key.code {
            KeyCode::Esc => return Modal::None,
            KeyCode::Tab | KeyCode::Down => form.focus = (form.focus + 1) % n,
            KeyCode::BackTab | KeyCode::Up => form.focus = (form.focus + n - 1) % n,
            KeyCode::Left if is_choice => form.fields[form.focus].cycle(-1),
            KeyCode::Right | KeyCode::Char(' ') if is_choice => form.fields[form.focus].cycle(1),
            KeyCode::Enter => {
                if form.focus + 1 < n && form.fields[form.focus + 1..].iter().any(|f| f.value.trim().is_empty() && !f.optional) {
                    form.focus += 1;
                } else {
                    match validate(&form).and_then(|()| self.check_available(&form)) {
                        Err((i, msg)) => {
                            form.focus = i;
                            form.error_field = Some(i);
                            form.error = Some(msg);
                        }
                        Ok(()) if matches!(form.kind, FormKind::OrderCreate { .. }) => {
                            let FormKind::OrderCreate { from, to, input, slippage } = &form.kind else { unreachable!() };
                            let account = form.fields.first().map(|f| f.value.trim().to_string()).filter(|s| !s.is_empty());
                            match super::order_ui::create_request(account, from.clone(), to.clone(), input.clone(), *slippage, &form.fields)
                            {
                                Ok(request) => {
                                    form.pending = true;
                                    self.send(Cmd::Order(request));
                                }
                                Err(e) => {
                                    form.error = Some(e.to_string());
                                    form.error_field = None;
                                }
                            }
                        }
                        Ok(()) if form.kind.is_local() => {
                            self.submit_form(&form);
                            return Modal::None;
                        }
                        Ok(()) if matches!(form.kind, FormKind::NftList { .. }) => {
                            let price = form.fields[0].value.trim().to_string();
                            if wallet_core::amount::parse_amount(&price, 18).is_err() || price.parse::<f64>().map_or(true, |p| p <= 0.0) {
                                form.focus = 0;
                                form.error_field = Some(0);
                                form.error = Some("prices are plain decimals above zero, like 250".into());
                            } else {
                                // A sequence (approvals, then the listing): the form closes and each step
                                // opens as its own review.
                                self.submit_form(&form);
                                return Modal::None;
                            }
                        }
                        Ok(()) => {
                            form.error = None;
                            form.error_field = None;
                            form.pending = true;
                            self.submit_form(&form);
                        }
                    }
                }
            }
            KeyCode::Backspace if !is_choice => {
                form.fields[form.focus].value.pop();
                form.error = None;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) && !is_choice => {
                form.fields[form.focus].value.zeroize();
            }
            KeyCode::Char(c) if !is_choice && !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if form.fields[form.focus].value.chars().count() < 512 {
                    form.fields[form.focus].value.push(c);
                }
                if form.error_field == Some(form.focus) {
                    form.error = None;
                    form.error_field = None;
                }
            }
            _ => {}
        }
        // Picking another function changes which arguments are needed.
        if matches!(form.kind, FormKind::ContractCall { .. }) && chosen(&form) != was_chosen {
            self.rebuild_contract_fields(&mut form);
        }
        self.probe_destination(&form);
        form.contract_note = Self::contract_note(&form.kind, self.contract_found.as_ref());
        Modal::Form(form)
    }

    /// What a probed destination turned out to be, as the line the form carries above its fields.
    /// `None` when there is nothing worth saying: a form with no destination in it, no answer yet,
    /// or an answer that came back a plain account.
    ///
    /// Takes the answer rather than reading `self.modal`, so both the key path (which has the form
    /// in hand) and the event path (which has it in `self.modal`) can use the one builder.
    fn contract_note(kind: &FormKind, found: Option<&wallet_core::contracts::Discovered>) -> Option<String> {
        Self::destination_field(kind)?;
        let found = found.filter(|f| f.is_contract())?;
        Some(match &found.metadata {
            Some(m) => format!("{} · a contract, not a wallet — ^F to call one of its functions", m.name),
            None => "a contract, not a wallet · it publishes no ABI, so it cannot be called from here".to_string(),
        })
    }

    fn on_screen_key(&mut self, key: KeyEvent, size: (u16, u16)) {
        if self.jump_pending.take().is_some() {
            if let KeyCode::Char(c) = key.code
                && let Some(idx) = label_index(c)
            {
                let target = self.view_offset() + idx;
                if target < self.list_len() {
                    self.selected = target;
                }
            }
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('p') => self.open_palette(),
                KeyCode::Char('d') => self.move_selection(10),
                KeyCode::Char('u') => self.move_selection(-10),
                KeyCode::Char('r') => self.send(Cmd::Refresh { full: true }),
                _ => {}
            }
            return;
        }
        // The pinned chat, when it has the keyboard, takes every key; Tab reaches it after the
        // screen's last pane or card field.
        if self.dock_focus && !self.dock_shown {
            self.dock_focus = false;
        }
        if self.dock_focus {
            self.dock_key(key);
            return;
        }
        if key.code == KeyCode::Tab && self.dock_shown && self.tab_reaches_dock() {
            self.dock_focus = true;
            return;
        }
        // Views with inline inputs (exchange cards, search) take their keys first.
        if self.detail.is_empty() && self.view_key(key) {
            return;
        }
        if !self.detail.is_empty() && self.detail_key(key) {
            return;
        }
        match key.code {
            KeyCode::Esc if !self.detail.is_empty() => {
                self.detail.pop();
                self.detail_selected = 0;
                self.kitty.clear(self.caps.tmux);
            }
            KeyCode::Char(':') => self.open_palette(),
            // Straight to the pinned chat's message box, or its form when it is not on screen.
            KeyCode::Char('`') if self.dock_shown => self.dock_focus = true,
            KeyCode::Char('`') => self.write_pinned(),
            KeyCode::Char('?') => self.modal = Modal::Help,
            KeyCode::Char('q') => {
                self.modal = Modal::Confirm {
                    title: "Quit".into(),
                    body: "Leave quai-terminal? Background tracking stops.".into(),
                    action: ConfirmAction::Quit,
                }
            }
            KeyCode::Char('l') => self.run_action("lock"),
            // Hide the balance on the top bar, for a room with other people in it.
            KeyCode::Char('$') => {
                self.config.balance_in_bar = !self.config.balance_in_bar;
                self.save_config();
                let on = self.config.balance_in_bar;
                self.toast(if on { "balance shown in the top bar" } else { "balance hidden ($ shows it)" }, false);
            }
            KeyCode::Char('[') => self.change_tab(-1),
            KeyCode::Char(']') => self.change_tab(1),
            KeyCode::Tab => self.pane = (self.pane + 1) % self.screen.panes().max(1),
            KeyCode::BackTab => self.pane = (self.pane + self.screen.panes().max(1) - 1) % self.screen.panes().max(1),
            KeyCode::Char(c) if Section::ALL.iter().any(|s| s.key() == c) => {
                if let Some(section) = Section::ALL.iter().copied().find(|s| s.key() == c) {
                    self.switch_section(section);
                }
            }
            KeyCode::Char(c @ ('6' | '7' | '8' | '9')) => {
                let hint = match c {
                    '6' => "Activity moved to 5; Qi coins, accounts and locks are under 1 Home",
                    '7' => "wrap moved to 2 Trade › Wrap",
                    '8' => "locks moved to 1 Home › Locks",
                    _ => "node moved to 0 System › Network",
                };
                self.toast(hint, false);
            }
            KeyCode::Char('\'') => self.jump_pending = Some('\''),
            KeyCode::Char('j') | KeyCode::Down => self.move_selection(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_selection(-1),
            KeyCode::Char('g') | KeyCode::Home => self.selected = 0,
            KeyCode::Char('G') | KeyCode::End => self.selected = self.list_len().saturating_sub(1),
            KeyCode::Enter => self.screen_enter(),
            KeyCode::Char('a') if self.screen == Screen::Wallets => self.begin_onboarding(OnboardKind::Create),
            KeyCode::Char('i') if self.screen == Screen::Wallets => self.begin_onboarding(OnboardKind::ImportPhrase),
            KeyCode::Char('e') if self.screen == Screen::Wallets => {
                if let Some(w) = self.wallets.get(self.selected).cloned() {
                    self.open_form(FormKind::RenameWallet(w.id));
                }
            }
            KeyCode::Char('o') => self.open_link(),
            KeyCode::Char('y') => {
                self.clipboard = self.selected_value();
                match &self.clipboard {
                    Some(v) => {
                        let shown = wallet_core::session::short_address(v);
                        self.toast(format!("copied {shown}"), false);
                    }
                    None => self.toast("nothing to copy here", true),
                }
            }
            KeyCode::Char('N') => {
                self.modal = Modal::Notifications;
                self.send(Cmd::MarkRead);
            }
            KeyCode::Char('R') => self.send(Cmd::Refresh { full: true }),
            KeyCode::Char('T') if self.screen != Screen::Collected => self.run_action("themes"),
            KeyCode::Char('L') => self.run_action("lock_gallery"),
            // ---------------- view actions
            KeyCode::Char('a') => match self.screen {
                Screen::Accounts => self.run_action("add_account"),
                Screen::Channels => self.save_channel_contact(),
                Screen::Contacts => self.run_action("add_contact"),
                Screen::Qi => self.run_action("new_qi_address"),
                _ => {}
            },
            KeyCode::Char('e') if self.screen == Screen::Accounts => {
                if let Some(a) = self.dash.accounts.get(self.selected) {
                    let (addr, label) = (a.address.clone(), a.label.clone());
                    self.open_form(FormKind::RenameAccount(addr));
                    if let Modal::Form(f) = &mut self.modal {
                        f.fields[0].value = label;
                    }
                }
            }
            KeyCode::Char('i') if self.screen == Screen::Home && self.pane == 0 => self.run_action("import_token"),
            KeyCode::Char('D') if self.screen == Screen::Home && self.pane == 0 => self.run_action("discover_tokens"),
            KeyCode::Char('p') if matches!(self.screen, Screen::Contacts | Screen::Channels) => self.run_action("add_peer"),
            KeyCode::Char('d') if matches!(self.screen, Screen::Contacts | Screen::Channels) => self.run_action("discover"),
            KeyCode::Char('e') if self.screen == Screen::Contacts => {
                if let Some(c) = self.dash.contacts.get(self.selected) {
                    let name = c.name.clone();
                    self.open_form(FormKind::Contact(Some(name)));
                }
            }
            KeyCode::Char('e') if self.screen == Screen::Channels => self.save_channel_contact(),
            KeyCode::Char('x') if self.screen == Screen::Channels => match self.channel_offer() {
                Some(o) => {
                    let body = format!(
                        "Decline {} and its {} Qi? It will not be offered again; adding the code as a peer is the only way back.",
                        wallet_core::session::short_code(&o.code),
                        wallet_core::amount::qi(o.found)
                    );
                    self.modal = Modal::Confirm {
                        title: "Decline payment channel".into(),
                        body,
                        action: ConfirmAction::DeclineOffer(o.code.clone()),
                    };
                }
                None => self.toast("x declines a channel offer; registered channels stay", false),
            },
            KeyCode::Char('S') if self.screen == Screen::Channels => {
                if let Some(p) = self.channel_peer() {
                    self.send(Cmd::ScanPeer(p.code.clone()));
                }
            }
            KeyCode::Char('n') if matches!(self.screen, Screen::Contacts | Screen::Channels) => {
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
            KeyCode::Char('Q') if self.screen == Screen::Contacts => {
                match self.dash.contacts.get(self.selected).and_then(|c| c.address.clone()) {
                    Some(address)
                        if wallet_core::registry::parse_any_address(&address)
                            .is_ok_and(|a| a.ledger() == wallet_core::sdk::Ledger::Quai) =>
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
            KeyCode::Char('x') if self.screen == Screen::Contacts => {
                if let Some(c) = self.dash.contacts.get(self.selected) {
                    self.modal = Modal::Confirm {
                        title: "Remove contact".into(),
                        body: format!("Remove `{}` from your address book? Payment-channel history is kept.", c.name),
                        action: ConfirmAction::RemoveContact(c.name.clone()),
                    };
                }
            }
            KeyCode::Char('S') if self.screen == Screen::Qi => self.run_action("scan_qi"),
            KeyCode::Char('D') if self.screen == Screen::Qi => self.run_action("deep_scan"),
            KeyCode::Char('A') if self.screen == Screen::Qi => self.run_action("aggregate"),
            KeyCode::Char('W') if self.screen == Screen::Qi => self.run_action("sweep"),
            KeyCode::Char('p') if self.screen == Screen::Activity => self.resume_trade_plan(),
            KeyCode::Char('u') if self.screen == Screen::Activity => self.run_action("speedup"),
            KeyCode::Char('t') if self.screen == Screen::DataSources => self.run_action("test_data"),
            // ---------------- global actions
            KeyCode::Char('s') => self.run_action(if matches!(self.screen, Screen::Qi | Screen::Contacts | Screen::Channels) {
                "send_qi"
            } else {
                "send_quai"
            }),
            KeyCode::Char('r') => {
                let qi = matches!(self.screen, Screen::Qi | Screen::Contacts | Screen::Channels);
                let account =
                    if self.screen == Screen::Accounts { self.selected.min(self.dash.accounts.len().saturating_sub(1)) } else { 0 };
                self.modal = Modal::Receive { asset_qi: qi, account };
            }
            KeyCode::Char('t') => self.run_action("trade"),
            KeyCode::Char('c') => self.run_action("convert_quai_qi"),
            KeyCode::Char('C') => self.run_action("convert_qi_quai"),
            _ => {}
        }
        let _ = size;
    }

    /// The sections in the sidebar: those with at least one view whose feature is on.
    pub fn sections(&self) -> Vec<Section> {
        Section::ALL.into_iter().filter(|s| !s.screens(&self.config.features).is_empty()).collect()
    }

    /// Switch to a section, restoring its last sub-tab (or its first, if that one is turned off).
    pub fn switch_section(&mut self, section: Section) {
        let screens = section.screens(&self.config.features);
        let Some(&first) = screens.first() else {
            if let Some(feature) = section.all_screens().iter().find_map(|s| s.feature()) {
                self.toast(format!("{} · System › Settings", feature.off_note()), false);
            }
            return;
        };
        let idx = Section::ALL.iter().position(|s| *s == section).unwrap_or(0);
        let last = section.all_screens().get(self.section_tabs[idx]).copied();
        self.switch(last.filter(|s| screens.contains(s)).unwrap_or(first));
    }

    /// `[` / `]`: previous or next sub-tab (Activity: filter).
    pub fn change_tab(&mut self, delta: i32) {
        let section = self.screen.section();
        if section == Section::Activity {
            let i = ActivityFilter::ALL.iter().position(|f| *f == self.activity_filter).unwrap_or(0) as i32;
            self.activity_filter = ActivityFilter::ALL[(i + delta).rem_euclid(ActivityFilter::ALL.len() as i32) as usize];
            self.selected = 0;
            return;
        }
        let screens = section.screens(&self.config.features);
        if screens.len() < 2 {
            return;
        }
        let i = screens.iter().position(|s| *s == self.screen).unwrap_or(0) as i32;
        self.switch(screens[(i + delta).rem_euclid(screens.len() as i32) as usize]);
    }

    /// Breadcrumb for the header: `Home › Portfolio › WQI`.
    pub fn breadcrumb(&self) -> Vec<String> {
        let section = self.screen.section();
        let mut parts = vec![section.title().to_string()];
        if section == Section::Activity {
            parts.push(self.activity_filter.title().to_string());
        } else if section.screens(&self.config.features).len() > 1 {
            parts.push(self.screen.title().to_string());
        }
        for d in &self.detail {
            parts.push(self.detail_title(d));
        }
        parts
    }

    fn open_link(&mut self) {
        let url = self.link_for_focus();
        match url {
            Some(u) => {
                self.clipboard = Some(u.clone());
                self.toast(format!("link copied · {u}"), false);
            }
            None => self.toast("no link for this item", true),
        }
    }

    fn screen_enter(&mut self) {
        match self.screen {
            Screen::Wallets => {
                if let Some(w) = self.wallets.get(self.selected).cloned() {
                    self.switch_wallet(&w.id);
                }
            }
            Screen::Network => {
                if let Some((id, name)) = self.dash.networks.get(self.selected).cloned() {
                    if id == self.dash.network_id {
                        self.toast(format!("already on {name}"), false);
                    } else {
                        let body = if id == "mainnet" {
                            format!("Switch to {name}? This uses real funds. It becomes your default network.")
                        } else {
                            format!("Switch to {name}? It becomes your default network.")
                        };
                        self.modal = Modal::Confirm { title: "Switch network".into(), body, action: ConfirmAction::SwitchNetwork(id) };
                    }
                }
            }
            Screen::Settings => self.settings_action(),
            Screen::DataSources => self.data_source_action(),
            Screen::Channels => {
                if let Some(o) = self.channel_offer() {
                    let who = wallet_core::session::short_code(&o.code);
                    let body = format!(
                        "{who} announced a payment channel, and {} Qi is waiting on it. Announcements are not authenticated: anyone can send one, and leave a little Qi to look real. Accept only a sender you expect; accepting adds the channel's Qi to this wallet and keeps scanning it.",
                        wallet_core::amount::qi(o.found)
                    );
                    self.modal =
                        Modal::Confirm { title: "Accept payment channel".into(), body, action: ConfirmAction::AcceptOffer(o.code.clone()) };
                } else if let Some(p) = self.channel_peer() {
                    let code = p.code.clone();
                    self.run_action("send_qi");
                    if let Modal::Form(f) = &mut self.modal {
                        f.fields[0].value = code;
                        f.focus = 1;
                    }
                }
            }
            Screen::Contacts => {
                if let Some(c) = self.dash.contacts.get(self.selected) {
                    let qi_address = c
                        .address
                        .as_deref()
                        .and_then(|a| wallet_core::registry::parse_any_address(a).ok())
                        .is_some_and(|a| a.ledger() == wallet_core::sdk::Ledger::Qi);
                    let form_kind = if c.payment_code.is_some() || qi_address { FormKind::SendQi } else { FormKind::SendQuai };
                    let name = c.name.clone();
                    self.open_form(form_kind.clone());
                    if let Modal::Form(f) = &mut self.modal {
                        // Contacts resolve by name, so the review shows the resolved destination.
                        let idx = if form_kind == FormKind::SendQi { 0 } else { 1 };
                        f.fields[idx].value = name;
                        f.focus = idx + 1;
                    }
                }
            }
            Screen::Activity => {
                if let Some(key) = self.activity_key(self.selected) {
                    self.push_detail(Detail::Activity(key));
                }
            }
            _ => self.enter_eco(),
        }
    }

    fn settings_action(&mut self) {
        let on_off = |b: bool| if b { "on" } else { "off" };
        let changed: Option<(String, String)> = match SETTINGS.get(self.selected).map(|s| s.0) {
            Some("theme") => {
                self.run_action("themes");
                None
            }
            Some("lock_effect") => {
                self.run_action("lock_gallery");
                None
            }
            Some("motion") => {
                self.config.motion = match self.config.motion {
                    Motion::Vivid => Motion::Full,
                    Motion::Full => Motion::Reduced,
                    Motion::Reduced => Motion::Off,
                    Motion::Off => Motion::Vivid,
                };
                Some(("Motion".into(), format!("{:?}", self.config.motion).to_lowercase()))
            }
            Some("layout") => {
                self.config.layout = match self.config.layout.as_str() {
                    "auto" => "standard",
                    "standard" => "trader",
                    "trader" => "focus",
                    _ => "auto",
                }
                .into();
                Some(("Layout".into(), self.config.layout.clone()))
            }
            Some(id) if let Some(feature) = Feature::ALL.into_iter().find(|f| id.strip_prefix("feature:") == Some(f.key())) => {
                let on = !self.config.features.on(feature);
                self.config.features.set(feature, on);
                if on {
                    // Warm what it shows now, rather than on the first visit.
                    self.eco.preloaded = false;
                    self.preload();
                }
                Some((feature.title().into(), if on { "on".into() } else { "off · hidden, and the daemon stops watching it".into() }))
            }
            Some("daemon") => {
                self.config.daemon_autostart = !self.config.daemon_autostart;
                let on = self.config.daemon_autostart;
                if on && !crate::daemon::daemon_running(&self.paths) {
                    let _ = crate::daemon::ensure_current(&self.paths, 20);
                }
                Some((
                    "Background daemon".into(),
                    if on { "starts with the terminal".into() } else { "off · quai-terminal daemon start".into() },
                ))
            }
            Some("daemon_unlock") => {
                self.config.daemon_share_unlock = !self.config.daemon_share_unlock;
                let on = self.config.daemon_share_unlock;
                Some((
                    "Unlock the daemon too".into(),
                    if on { "on · from the next unlock".into() } else { "off · quai-terminal daemon unlock".into() },
                ))
            }
            Some("ceremonies") => {
                self.config.ceremonies = !self.config.ceremonies;
                Some(("Effects & celebrations".into(), on_off(self.config.ceremonies).into()))
            }
            Some("sound") => {
                self.config.sound = !self.config.sound;
                self.bell = self.config.sound;
                Some(("Terminal bell".into(), on_off(self.config.sound).into()))
            }
            Some("big_numbers") => {
                self.config.big_numbers = !self.config.big_numbers;
                Some(("Big balance digits".into(), on_off(self.config.big_numbers).into()))
            }
            Some("balance_in_bar") => {
                self.config.balance_in_bar = !self.config.balance_in_bar;
                Some(("Balance in the top bar".into(), on_off(self.config.balance_in_bar).into()))
            }
            Some("notifications") => {
                self.config.notifications = !self.config.notifications;
                Some(("Notifications".into(), on_off(self.config.notifications).into()))
            }
            Some("autolock") => {
                self.config.auto_lock_minutes = match self.config.auto_lock_minutes {
                    0 => 5,
                    5 => 10,
                    10 => 30,
                    30 => 60,
                    _ => 0,
                };
                let v =
                    if self.config.auto_lock_minutes == 0 { "off".to_string() } else { format!("{} min", self.config.auto_lock_minutes) };
                Some(("Auto-lock".into(), v))
            }
            Some("images") => {
                self.config.images = !self.config.images;
                self.config.token_icons = self.config.images;
                self.data_policy_changed();
                Some(("Images".into(), on_off(self.config.images).into()))
            }
            Some("ipfs") => {
                self.open_form(FormKind::IpfsGateway(wallet_core::ipfs::Content::Media));
                None
            }
            Some("abi_ipfs") => {
                self.open_form(FormKind::IpfsGateway(wallet_core::ipfs::Content::Abi));
                None
            }
            Some("phrase") => {
                self.run_action("export_phrase");
                None
            }
            Some("backup") => {
                self.run_action("backup");
                None
            }
            Some("refresh") => {
                self.run_action("refresh");
                None
            }
            _ => None,
        };
        if let Some((name, value)) = changed {
            self.save_config();
            // Every change is announced, so an accidental Enter never goes unnoticed.
            self.toast(format!("{name} · {value}  (enter again to change)"), false);
        }
    }

    fn data_source_action(&mut self) {
        let on_off = |b: bool| if b { "on" } else { "off" };
        let changed: Option<(String, String)> = match DATA_SOURCES.get(self.selected).map(|s| s.0) {
            Some("explorer_lookups") => {
                self.config.explorer_lookups = !self.config.explorer_lookups;
                Some(("Explorer lookups".into(), on_off(self.config.explorer_lookups).into()))
            }
            Some("market_data") => {
                self.config.fetch_prices = !self.config.fetch_prices;
                Some(("Market data".into(), on_off(self.config.fetch_prices).into()))
            }
            Some("images") => {
                self.config.images = !self.config.images;
                Some(("NFT images".into(), on_off(self.config.images).into()))
            }
            Some("token_icons") => {
                self.config.token_icons = !self.config.token_icons;
                Some(("Token icons".into(), on_off(self.config.token_icons).into()))
            }
            Some("test") => {
                self.run_action("test_data");
                None
            }
            _ => None,
        };
        if let Some((name, value)) = changed {
            self.save_config();
            self.data_policy_changed();
            self.toast(format!("{name} · {value}"), false);
        }
    }

    pub fn save_config(&mut self) {
        if let Err(e) = self.config.save(&self.paths) {
            self.toast(e.to_string(), true);
        }
    }

    pub fn switch(&mut self, screen: Screen) {
        if let Some(feature) = screen.feature().filter(|f| !self.config.features.on(*f)) {
            self.toast(format!("{} · System › Settings", feature.off_note()), false);
            return;
        }
        // Leaving Markets from its pair list: the chart keeps that pair wherever it is drawn next.
        if self.screen == Screen::Markets && self.pane == 0 && screen != Screen::Markets {
            self.eco.markets_view.pair_selected = self.selected;
        }
        let section = screen.section();
        if let (Some(si), Some(ti)) =
            (Section::ALL.iter().position(|s| *s == section), section.all_screens().iter().position(|s| *s == screen))
        {
            self.section_tabs[si] = ti;
        }
        if self.screen != screen || !self.detail.is_empty() {
            self.kitty.clear(self.caps.tmux);
            self.screen = screen;
            self.selected = 0;
            self.pane = 0;
            self.detail.clear();
            self.detail_selected = 0;
            if screen == Screen::Network {
                self.selected = self.dash.networks.iter().position(|(id, _)| *id == self.dash.network_id).unwrap_or(0);
            }
            self.start_transition();
            self.unfocus_cards();
            self.on_view_opened();
        }
    }

    /// The channel offer under the cursor on Channels (offers are listed first).
    pub(crate) fn channel_offer(&self) -> Option<&wallet_core::ops::ChannelOffer> {
        self.dash.offers.get(self.selected)
    }

    /// The registered channel under the cursor on Channels, below the offers.
    pub(crate) fn channel_peer(&self) -> Option<&wallet_core::ops::PeerView> {
        self.selected.checked_sub(self.dash.offers.len()).and_then(|i| self.dash.peers.get(i))
    }

    /// Save a payment channel's sender as a contact, or edit the contact it already belongs to.
    fn save_channel_contact(&mut self) {
        let Some(p) = self.channel_peer() else { return };
        let code = p.code.clone();
        match self.dash.contacts.iter().find(|c| c.payment_code.as_deref() == Some(code.as_str())).map(|c| c.name.clone()) {
            Some(name) => self.open_form(FormKind::Contact(Some(name))),
            None => {
                self.open_form(FormKind::Contact(None));
                if let Modal::Form(f) = &mut self.modal {
                    f.fields[2].value = code;
                    f.focus = 0;
                }
            }
        }
    }

    /// Switch the worker to another network; balances from the old one are cleared immediately.
    fn switch_network(&mut self, id: String) {
        let name = self.dash.networks.iter().find(|(n, _)| *n == id).map(|(_, name)| name.clone()).unwrap_or_else(|| id.clone());
        self.network_id = id.clone();
        self.config.default_network = id.clone();
        self.save_config();
        let networks = std::mem::take(&mut self.dash.networks);
        self.dash = Dashboard {
            meta: self.dash.meta.clone(),
            network_id: id.clone(),
            network_name: name.clone(),
            unlocked: self.dash.unlocked,
            networks,
            ..Dashboard::default()
        };
        self.busy = Some(format!("connecting to {name}…"));
        self.send(Cmd::SwitchNetwork(id.clone()));
        self.reset_eco_for_network();
        let _ = id;
    }

    /// First visible row index for jump labels (lists scroll with the selection).
    pub fn view_offset(&self) -> usize {
        self.selected.saturating_sub(self.selected % 36)
    }

    fn receive_value(&self, asset_qi: bool, account: usize) -> Option<String> {
        if asset_qi {
            self.meta.as_ref().and_then(|m| m.payment_code.clone()).or_else(|| self.dash.qi_addresses.last().map(|(_, a, _)| a.clone()))
        } else {
            self.dash.accounts.get(account).map(|a| a.address.clone())
        }
    }

    /// The most useful string on the selected row (address, hash or code) for `y`.
    fn selected_value(&self) -> Option<String> {
        if let Some(v) = self.eco_selected_value() {
            return Some(v);
        }
        match self.screen {
            Screen::Accounts => self.dash.accounts.get(self.selected).map(|a| a.address.clone()),
            Screen::Activity => match self.activity_rows().get(self.selected) {
                Some((_, true, i)) => self.dash.ops[*i].tx_hash.clone(),
                Some((_, false, i)) => self.dash.activity[*i].tx_hash.clone(),
                None => None,
            },
            Screen::Qi => self.dash.qi.as_ref().and_then(|q| q.coins.get(self.selected)).map(|c| c.address.clone()),
            Screen::Channels => self.channel_offer().map(|o| o.code.clone()).or_else(|| self.channel_peer().map(|p| p.code.clone())),
            Screen::Contacts => match self.dash.contacts.get(self.selected) {
                Some(c) => c.payment_code.clone().or_else(|| c.address.clone()),
                None => self.meta.as_ref().and_then(|m| m.payment_code.clone()),
            },
            _ => self.receive_value(false, 0),
        }
    }

    /// Catch amounts above the known spendable balance before asking the node.
    fn check_available(&self, form: &Form) -> Result<(), (usize, String)> {
        let account =
            form.fields.iter().find(|f| matches!(f.kind, FieldKind::Choice(_)) && f.value.starts_with("0x")).map(|f| f.value.clone());
        for (i, f) in form.fields.iter().enumerate() {
            let FieldKind::Amount(asset) = f.kind else { continue };
            let (have, decimals, unit) = match asset {
                "QUAI" => {
                    let a =
                        account.as_ref().and_then(|v| self.dash.accounts.iter().find(|a| a.address == *v)).or(self.dash.accounts.first());
                    match a {
                        Some(a) => (a.balance, 18, "QUAI"),
                        None => continue,
                    }
                }
                "QI" => match &self.dash.qi {
                    Some(q) => (q.balance.spendable, 3, "Qi"),
                    None => continue,
                },
                _ => continue,
            };
            let Ok(want) = wallet_core::amount::parse_amount(f.value.trim(), decimals) else { continue };
            if want > have {
                let shown = wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(have, decimals, 4));
                return Err((i, format!("more than the {shown} {unit} available")));
            }
        }
        Ok(())
    }

    /// Set (after verifying chain id and genesis) or clear (`url` empty) a monitoring endpoint.
    fn set_monitor(&mut self, network: &str, url: &str, pathing: bool) {
        if url.is_empty() {
            if self.config.monitor_endpoints.remove(network).is_some() {
                self.save_config();
                self.data_policy_changed();
            }
            self.toast(format!("{network}: monitoring uses the main RPC"), false);
            return;
        }
        let profile = match self.config.network(network) {
            Ok(mut p) => {
                p.monitor = Some(wallet_core::network::MonitorEndpoint { rpc_url: url.to_string(), use_pathing: pathing });
                p
            }
            Err(e) => return self.toast(e.to_string(), true),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.monitor_check = Some(rx);
        self.busy = Some(format!("checking {url} against {network}…"));
        let network = network.to_string();
        std::thread::spawn(move || {
            let endpoint = profile.monitor.clone().expect("set above");
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|rt| {
                rt.block_on(async {
                    let node = profile.monitor_node()?;
                    wallet_core::network::require_identity(&profile, &node.provider).await
                })
                .map_err(|e| e.to_string())
            });
            let _ = tx.send((network, endpoint, result));
        });
    }

    /// Test an IPFS gateway on its own thread, then save it (`url` empty: back to ipfs.io).
    ///
    /// Open the call form for a contract the send form found, on its first writable function.
    /// `false` when there was nothing to open, so the caller can leave the form it was on alone
    /// rather than closing it over a toast.
    ///
    /// Takes the answer by value: it can carry the contract's whole literal source.
    pub(crate) fn open_contract_call(&mut self, found: wallet_core::contracts::Discovered) -> bool {
        let Some(metadata) = &found.metadata else { return false };
        let Ok(interface) = metadata.interface() else {
            self.toast("this contract's ABI could not be read", true);
            return false;
        };
        // Reads are answered, not signed; this form is for the ones that cost something.
        let functions: Vec<wallet_core::contracts::Callable> =
            wallet_core::contracts::callables(&interface).into_iter().filter(|c| !c.read_only).collect();
        if functions.is_empty() {
            self.toast(format!("{} declares nothing that can be called", metadata.name), true);
            return false;
        }
        self.open_form(FormKind::ContractCall { address: found.address.clone(), name: metadata.name.clone(), functions });
        // `open_form` clears the probe; this form is about that contract, so it keeps it.
        self.contract_found = Some(found);
        true
    }

    /// Rebuild the argument fields under the chosen function, keeping the account and any value
    /// already typed. Each function needs its own arguments, so the form changes shape with it.
    fn rebuild_contract_fields(&mut self, form: &mut Form) {
        let FormKind::ContractCall { functions, .. } = &form.kind else { return };
        let Some(chosen) = form.fields.iter().find(|f| f.label == "Function").map(|f| f.value.clone()) else { return };
        let Some(callable) = functions.iter().find(|c| c.signature == chosen).cloned() else { return };
        let keep = |label: &str| form.fields.iter().find(|f| f.label == label).map(|f| f.value.clone()).unwrap_or_default();
        let (account, value) = (keep("From"), keep("QUAI to send"));
        let mut fields: Vec<Field> = form.fields.iter().take(2).cloned().collect();
        if callable.payable {
            fields.push(Field::new("QUAI to send", "this function accepts QUAI").with(value).optional().amount("QUAI"));
        }
        for (name, ty) in &callable.inputs {
            let label = if name.is_empty() { ty.clone() } else { format!("{name} ({ty})") };
            fields.push(Field::new(&label, &argument_hint(ty)));
        }
        if let Some(f) = fields.first_mut() {
            f.value = account;
        }
        form.focus = form.focus.min(fields.len().saturating_sub(1));
        form.fields = fields;
        form.error = None;
        form.error_field = None;
    }

    /// Which form field holds a destination worth asking the chain about, if any.
    fn destination_field(kind: &FormKind) -> Option<&'static str> {
        match kind {
            FormKind::SendQuai | FormKind::SendToken => Some("To"),
            FormKind::Approve => Some("Spender"),
            _ => None,
        }
    }

    /// Ask what a send destination is, once it looks like a finished address. Sending QUAI to a
    /// contract with no payable fallback burns the fee for nothing, and a contract someone means
    /// to *use* needs a different form than a transfer — so the form finds out while they type
    /// rather than after they have signed.
    pub(crate) fn probe_destination(&mut self, form: &Form) {
        let Some(label) = Self::destination_field(&form.kind) else { return };
        let Some(text) = form.fields.iter().find(|f| f.label == label).map(|f| f.value.trim().to_string()) else { return };
        // Only a complete address; a contact name resolves to one the worker looks up itself.
        let looks_done = text.len() >= 42 && text.starts_with("0x");
        if !looks_done {
            self.contract_found = None;
            self.contract_probe = None;
            return;
        }
        // Asked once per destination per form, whatever the answer was. A failure is an answer.
        if self.contract_probe.as_deref() == Some(text.as_str()) || !self.contract_asked.insert(text.to_lowercase()) {
            return;
        }
        self.contract_found = None;
        self.contract_probe = Some(text.clone());
        self.send(Cmd::InspectContract { address: text });
    }

    /// Put what the probe found under the open form, without disturbing what is typed in it.
    pub(crate) fn refresh_form_note(&mut self) {
        let Modal::Form(mut form) = std::mem::replace(&mut self.modal, Modal::None) else { return };
        form.contract_note = Self::contract_note(&form.kind, self.contract_found.as_ref());
        self.modal = Modal::Form(form);
        self.dirty = true;
    }

    /// Test an IPFS gateway on its own thread, then save it if it answers for the content it is
    /// being set for (empty goes back to the built-in one, which needs no test).
    ///
    /// Saved when it serves the test file byte for byte, and also when it answers but cannot find
    /// the file in time — a node that has just started may need a while to reach the network, and
    /// that is not a reason to refuse it. Not saved when it cannot be reached at all, or when it
    /// returns content that does not match the CID it was asked for.
    fn set_ipfs_gateway(&mut self, content: wallet_core::ipfs::Content, url: &str) {
        // Empty means "back to the built-in gateway for this content", which needs no test.
        let url = url.trim();
        if url.is_empty() || url == "default" {
            match content {
                wallet_core::ipfs::Content::Abi => self.config.abi_ipfs_gateway = None,
                wallet_core::ipfs::Content::Media => self.config.ipfs_gateway = None,
            }
            let _ = wallet_core::ipfs::set_gateway(content, None);
            self.save_config();
            self.eco.images.retain(|_, slot| !matches!(slot, super::eco::ImageSlot::Failed(_)));
            return self.toast(format!("{} now uses {}", content.label(), content.default_gateway()), false);
        }
        let gateway = match wallet_core::ipfs::Gateway::parse(url) {
            Ok(g) => g,
            Err(e) => return self.toast(e.to_string(), true),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        self.ipfs_check = Some(rx);
        self.busy = Some(format!("testing {}…", gateway.display()));
        std::thread::spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(wallet_core::ipfs::test(&gateway)).map_err(|e| e.to_string()));
            let _ = tx.send((content, gateway, result));
        });
    }

    /// Finish an IPFS gateway test.
    pub fn poll_ipfs_check(&mut self) {
        let Some(rx) = &self.ipfs_check else { return };
        let Ok((content, gateway, result)) = rx.try_recv() else { return };
        self.ipfs_check = None;
        self.busy = None;
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => return self.toast(format!("IPFS gateway not saved: {e}"), true),
        };
        let stored = (!gateway.is_default_for(content)).then(|| gateway.display());
        match content {
            wallet_core::ipfs::Content::Abi => self.config.abi_ipfs_gateway = stored.clone(),
            wallet_core::ipfs::Content::Media => self.config.ipfs_gateway = stored.clone(),
        }
        let _ = wallet_core::ipfs::set_gateway(content, stored.as_deref());
        self.save_config();
        // Pictures that failed on the old gateway get another chance on this one.
        self.eco.images.retain(|_, slot| !matches!(slot, super::eco::ImageSlot::Failed(_)));
        match outcome {
            wallet_core::ipfs::TestOutcome::Verified(ms) => {
                self.toast(format!("IPFS via {} · test file verified against its CID in {ms} ms", gateway.display()), false)
            }
            wallet_core::ipfs::TestOutcome::Answered(why) => {
                self.toast(format!("IPFS via {} · saved; it answered but the test file did not arrive ({why})", gateway.display()), true)
            }
        }
    }

    /// Finish a monitoring endpoint check.
    pub fn poll_monitor_check(&mut self) {
        let Some(rx) = &self.monitor_check else { return };
        let Ok((network, endpoint, result)) = rx.try_recv() else { return };
        self.monitor_check = None;
        self.busy = None;
        match result {
            Ok(()) => {
                let url = endpoint.rpc_url.clone();
                self.config.monitor_endpoints.insert(network.clone(), endpoint);
                self.save_config();
                if network == self.network_id {
                    self.data_policy_changed();
                }
                self.toast(format!("{network}: monitoring via {url} (chain id and genesis verified)"), false);
            }
            Err(e) => self.toast(format!("monitoring endpoint not saved: {e}"), true),
        }
    }

    /// Add another wallet through the same flow the first one used: it shows and verifies a
    /// recovery phrase, or takes one being imported, and names it.
    pub fn begin_onboarding(&mut self, kind: OnboardKind) {
        self.onboarding = Some(super::onboarding::start(self, kind));
        self.dirty = true;
    }

    /// The wallets on this computer, newest last, for the Wallets screen.
    pub fn load_wallets(&mut self) {
        self.wallets = self.registry.list().unwrap_or_default();
        let network = self.network_id.clone();
        self.wallet_summaries = self
            .wallets
            .iter()
            .filter_map(|w| wallet_core::cockpit::load_summary(&self.paths, &w.id, &network).map(|s| (w.id.clone(), s)))
            .collect();
        let addresses: Vec<(String, Vec<String>)> = self.wallets.iter().map(|w| (w.id.clone(), w.quai_owner_addresses())).collect();
        if !addresses.is_empty() {
            self.send_data(super::data::DataCmd::WalletQuai(addresses));
        }
    }

    /// Check a password on its own thread. The lock screen says it is unlocking meanwhile, and
    /// [`App::poll_unlock`] takes the answer.
    pub fn begin_unlock(&mut self, password: Zeroizing<String>) {
        let Some(meta) = self.meta.clone() else { return };
        self.unlocking = true;
        self.unlocking_since = Some(Instant::now());
        self.lock_error = None;
        self.dirty = true;
        let (tx, rx) = std::sync::mpsc::channel();
        let registry = self.registry.clone();
        let wallet = meta.id.clone();
        let spawned = std::thread::Builder::new().name("wallet-unlock".into()).spawn(move || {
            let answer = registry.unlock(&meta, &password).map(|keys| (keys, password)).map_err(|e| e.to_string());
            let _ = tx.send(answer);
        });
        match spawned {
            Ok(_) => self.unlock_check = Some((wallet, rx)),
            Err(e) => {
                self.unlocking = false;
                self.unlocking_since = None;
                self.lock_error = Some(format!("could not start unlocking: {e}"));
            }
        }
    }

    /// Take a finished password check: unlock the screen and hand the keys to the worker, or say
    /// why not.
    pub fn poll_unlock(&mut self) {
        let Some((wallet, rx)) = &self.unlock_check else { return };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("unlocking stopped unexpectedly — try again".into()),
        };
        let wallet = wallet.clone();
        self.unlock_check = None;
        self.unlocking = false;
        self.unlocking_since = None;
        self.dirty = true;
        // Checked for a wallet that is no longer the open one: its keys are dropped here.
        if !self.locked || self.meta.as_ref().is_none_or(|m| m.id != wallet) {
            return;
        }
        match answer {
            Ok((keys, password)) => {
                if self.config.daemon_share_unlock && crate::daemon::state(&self.paths).is_some_and(|d| !d.unlocked(&wallet)) {
                    self.hand_to_daemon(wallet.clone(), password.clone());
                }
                self.send(Cmd::UseKeys { wallet, keys: Box::new(keys), password });
                self.show_unlocked();
            }
            Err(e) => {
                let text = friendly_error(&e);
                self.lock_error = Some(text.clone());
                self.log.push_front(Toast { text, error: true, at: Instant::now() });
            }
        }
    }

    /// Give the running daemon this wallet's password, off the UI thread. The copy lives only in
    /// that task and is wiped when it ends; every check `daemon::hand_unlock` makes still applies.
    pub(crate) fn hand_to_daemon(&mut self, wallet: String, password: Zeroizing<String>) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else { return };
        let (tx, rx) = std::sync::mpsc::channel();
        let paths = self.paths.clone();
        runtime.spawn(async move {
            let answer = crate::daemon::hand_unlock(&paths, &wallet, &password).await.map_err(|e| e.to_string());
            drop(password);
            let _ = tx.send(answer);
        });
        self.handoff = Some(rx);
    }

    /// Say how a hand-off to the daemon went.
    pub(crate) fn poll_handoff(&mut self) {
        let Some(rx) = &self.handoff else { return };
        match rx.try_recv() {
            Ok(Ok(name)) => {
                self.handoff = None;
                self.toast(format!("{name} is unlocked in the daemon too · quai-terminal daemon lock"), false);
            }
            Ok(Err(e)) => {
                self.handoff = None;
                self.toast(e, true);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.handoff = None,
        }
    }

    /// Leave the lock screen for the dashboard.
    fn show_unlocked(&mut self) {
        self.locked = false;
        self.unlocking = false;
        self.unlocking_since = None;
        self.lock_input.zeroize();
        self.lock_error = None;
        self.ambient = None;
        if let Some(form) = self.parked.take() {
            self.modal = Modal::Form(form);
        }
        self.unlocked_at = Some(Instant::now());
        self.ceremony = None;
        // A quick reveal of the dashboard instead of an overlay on top of it.
        if self.motion().effects() {
            let c = self.theme.surface;
            self.transition = Some(tachyonfx::fx::fade_from(c, c, (300, tachyonfx::Interpolation::QuadOut)));
        }
        self.send(Cmd::Refresh { full: false });
    }

    /// Finish background wallet creation.
    pub fn poll_creation(&mut self) {
        let Some(rx) = &self.creating else { return };
        match rx.try_recv() {
            Ok(Ok((meta, password))) => {
                self.creating = None;
                self.busy = None;
                if self.config.default_wallet.is_none() {
                    self.config.default_wallet = Some(meta.name.clone());
                }
                self.config.default_network = self.network_id.clone();
                self.config.onboarded = true;
                self.save_config();
                self.locked = meta.kind != WalletKind::Watch;
                self.pending_unlock = password;
                self.toast(format!("wallet `{}` is ready", meta.name), false);
                self.onboarding = None;
                // A wallet added while one is already open: the session has to follow it, or the
                // screen would show the new name over the old wallet's balances and history.
                // `switch_wallet` reads the new wallet itself, so `meta` stays the old one until
                // it does — otherwise it would see no change and do nothing.
                if self.worker.is_some() {
                    let id = meta.id.clone();
                    self.switch_wallet(&id);
                    // They typed this password a moment ago; do not ask for it again.
                    if let Some(p) = self.pending_unlock.take() {
                        self.begin_unlock(p);
                    }
                } else {
                    self.meta = Some(meta);
                }
                self.load_wallets();
                self.dirty = true;
            }
            Ok(Err(e)) => {
                self.creating = None;
                self.busy = None;
                self.toast(e, true);
                self.dirty = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.creating = None;
                self.busy = None;
            }
        }
    }

    /// Housekeeping every ~500ms: auto-lock, toast expiry, idle lock-screen effect.
    pub fn tick(&mut self, size: (u16, u16)) {
        if let Some(left) = self.autolock_remaining()
            && left <= 60
            && left > 0
            && !self.lock_warned
        {
            self.lock_warned = true;
            self.bell = self.config.sound;
            self.toast(format!("locking in {left}s — press any key to stay unlocked"), true);
        }
        if self.autolock_remaining() == Some(0) {
            self.lock_now(Some(size));
            self.last_input = Instant::now();
        }
        let before = self.toasts.len();
        self.toasts.retain(|t| t.at.elapsed().as_secs() < if t.error { 15 } else { 6 });
        if before != self.toasts.len() {
            self.dirty = true;
        }
        // A lock screen that isn't being drawn (no size yet, effects just re-enabled) still gets
        // its ceremony here. While it *is* drawn, the renderer chains one effect into the next.
        if self.locked && self.ambient.is_none() && self.meta.is_some() {
            self.start_lock_ceremony(size);
        }
        if matches!(self.modal, Modal::Effects(_)) {
            self.dirty = true;
        }
        // An unlock that never answers must not leave the screen ignoring the keyboard.
        if let Some(at) = self.unlocking_since
            && at.elapsed() > std::time::Duration::from_secs(30)
        {
            self.unlocking = false;
            self.unlocking_since = None;
            // A late answer is dropped with it, so the next attempt starts clean.
            self.unlock_check = None;
            self.lock_error = Some("the wallet did not answer — try again".into());
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
        "wrong password, or this vault file is damaged".into()
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
        "trade" | "swap" | "launches" => Some(Feature::Trading),
        "nfts" | "explore" | "listings" => Some(Feature::Nfts),
        _ => None,
    }
}

/// Settings rows: (id, label).
pub const SETTINGS: &[(&str, &str)] = &[
    ("theme", "Theme"),
    ("motion", "Motion"),
    ("layout", "Layout"),
    ("feature:messaging", "Messaging"),
    ("feature:trading", "Trading"),
    ("feature:nfts", "NFTs"),
    ("daemon", "Background daemon"),
    ("daemon_unlock", "Unlock the daemon too"),
    ("ceremonies", "Effects & celebrations"),
    ("lock_effect", "Lock screen animation"),
    ("sound", "Terminal bell on good news"),
    ("big_numbers", "Big balance digits"),
    ("balance_in_bar", "Balance in the top bar"),
    ("notifications", "Notifications"),
    ("autolock", "Auto-lock"),
    ("images", "NFT images and token icons"),
    ("ipfs", "IPFS gateway · images"),
    ("abi_ipfs", "IPFS gateway · contract ABIs"),
    ("phrase", "Reveal recovery phrase…"),
    ("backup", "Encrypted backup…"),
    ("refresh", "Full refresh"),
];

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
#[path = "app_tests.rs"]
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
