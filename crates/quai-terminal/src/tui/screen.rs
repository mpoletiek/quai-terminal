//! Screens, one impl each (A8 in the architecture review): what a screen is called, which
//! feature and mode it belongs to, its panes and list, what it loads when it opens, the keys it
//! takes itself, and how it draws. Adding a screen is a [`Screen`] variant, an impl here, and its
//! row in the section table; nothing else dispatches on which screen is open for these.

use super::app::{App, Screen};
use super::theme::Theme;
use crossterm::event::KeyEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use wallet_core::config::Feature;

/// One screen.
pub trait ScreenView: Sync {
    fn title(&self) -> &'static str;
    /// The optional part of the wallet it belongs to, when it does.
    fn feature(&self) -> Option<Feature> {
        None
    }
    /// Part of Pro: hidden while the terminal is Simple.
    fn pro_only(&self) -> bool {
        false
    }
    /// Panes Tab and Shift-Tab move between.
    fn panes(&self) -> usize {
        1
    }
    /// Rows in the list that has the keys.
    fn list_len(&self, _app: &App) -> usize {
        0
    }
    /// What to load when it opens.
    fn on_open(&self, _app: &mut App) {}
    /// A key it takes itself, before the app's meaning of it. True when taken.
    fn key(&self, _app: &mut App, _key: KeyEvent) -> bool {
        false
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect);
}

/// The impl of a screen.
pub fn view(screen: Screen) -> &'static dyn ScreenView {
    match screen {
        Screen::Home => &Home,
        Screen::Qi => &Qi,
        Screen::Accounts => &Accounts,
        Screen::Markets => &Markets,
        Screen::Exchange => &Exchange,
        Screen::Pools => &Pools,
        Screen::Orders => &Orders,
        Screen::Launches => &Launches,
        Screen::Pnl => &Pnl,
        Screen::Collected => &Collected,
        Screen::Explore => &Explore,
        Screen::Listings => &Listings,
        Screen::Contacts => &Contacts,
        Screen::Board => &Board,
        Screen::Wallets => &Wallets,
        Screen::Activity => &Activity,
        Screen::Network => &Network,
        Screen::Settings => &Settings,
        Screen::DataSources => &DataSources,
    }
}

use super::ui::screens as ui;
use super::views;

struct Home;
impl ScreenView for Home {
    fn title(&self) -> &'static str {
        "Portfolio"
    }
    fn panes(&self) -> usize {
        2
    }
    fn list_len(&self, app: &App) -> usize {
        if app.nav.pane == 1 {
            app.activity_rows().len().min(12)
        } else {
            app.eco.feeds.portfolio.value().map_or(0, |p| p.rows.len()) + app.home_positions().len()
        }
    }
    fn on_open(&self, app: &mut App) {
        app.maybe_refresh_portfolio(false);
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.home_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_home(f, app, t, area);
    }
}

struct Qi;
impl ScreenView for Qi {
    fn title(&self) -> &'static str {
        "Qi coins"
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.dash.qi.as_ref().map_or(0, |q| q.coins.len())
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_qi(f, app, t, area);
    }
}

/// Accounts, with the wallet's time locks beneath them.
struct Accounts;
impl ScreenView for Accounts {
    fn title(&self) -> &'static str {
        "Accounts"
    }
    fn list_len(&self, app: &App) -> usize {
        app.dash.accounts.len()
    }
    fn on_open(&self, app: &mut App) {
        app.open_accounts();
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_accounts(f, app, t, area);
    }
}

struct Markets;
impl ScreenView for Markets {
    fn title(&self) -> &'static str {
        "Pairs"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Trading)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn panes(&self) -> usize {
        2
    }
    fn list_len(&self, app: &App) -> usize {
        if app.nav.pane == 1 { app.flow_rows().len() } else { app.market_rows().len() }
    }
    fn on_open(&self, app: &mut App) {
        app.open_markets();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.markets_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_markets(f, app, t, area);
    }
}

/// Swap, convert and wrap: one screen, whose card the pair decides. Converting and wrapping are
/// wallet operations, so it stays without trading (on the conversion card).
struct Exchange;
impl ScreenView for Exchange {
    fn title(&self) -> &'static str {
        "Exchange"
    }
    fn on_open(&self, app: &mut App) {
        app.open_exchange();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.exchange_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        use super::app::Card;
        match app.nav.card {
            Card::Swap => views::draw_swap(f, app, t, area),
            Card::Convert => views::draw_convert_card(f, app, t, area),
            Card::Wrap => views::draw_wrap_card(f, app, t, area),
        }
    }
}

/// Liquidity positions, adding, removing and gauge staking.
struct Pools;
impl ScreenView for Pools {
    fn title(&self) -> &'static str {
        "Pools"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Trading)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn panes(&self) -> usize {
        2
    }
    fn list_len(&self, app: &App) -> usize {
        app.eco.pools_view.positions.value().map_or(0, Vec::len)
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.pools_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_pools(f, app, t, area);
    }
}

/// Limit orders: watch, review, cancel.
struct Orders;
impl ScreenView for Orders {
    fn title(&self) -> &'static str {
        "Orders"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Trading)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.order_rows().len()
    }
    fn on_open(&self, app: &mut App) {
        app.orders_list();
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        super::order_ui::draw_screen(f, app, t, area);
    }
}

/// Quainance's launch zone: bonding-curve launches and where they trade now.
struct Launches;
impl ScreenView for Launches {
    fn title(&self) -> &'static str {
        "Launches"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Trading)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.launch_rows().len()
    }
    fn on_open(&self, app: &mut App) {
        app.load_launches(false);
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.launches_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_launches(f, app, t, area);
    }
}

/// Trading performance in QUAI from this wallet's own trades.
struct Pnl;
impl ScreenView for Pnl {
    fn title(&self) -> &'static str {
        "PnL"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Trading)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.pnl_positions().len()
    }
    fn on_open(&self, app: &mut App) {
        app.load_pnl(false);
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.pnl_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_pnl(f, app, t, area);
    }
}

struct Collected;
impl ScreenView for Collected {
    fn title(&self) -> &'static str {
        "Collected"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Nfts)
    }
    fn list_len(&self, app: &App) -> usize {
        app.eco.nft_len()
    }
    fn on_open(&self, app: &mut App) {
        app.open_collected();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.collected_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_collected(f, app, t, area);
    }
}

struct Explore;
impl ScreenView for Explore {
    fn title(&self) -> &'static str {
        "Explore"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Nfts)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.eco.collections_filtered().len()
    }
    fn on_open(&self, app: &mut App) {
        app.open_explore();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.explore_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_explore(f, app, t, area);
    }
}

struct Listings;
impl ScreenView for Listings {
    fn title(&self) -> &'static str {
        "Listings"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Nfts)
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.eco.listings_len()
    }
    fn on_open(&self, app: &mut App) {
        app.open_listings();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.listings_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_listings(f, app, t, area);
    }
}

/// Contacts, and below them the payment channels with them.
struct Contacts;
impl ScreenView for Contacts {
    fn title(&self) -> &'static str {
        "Contacts"
    }
    fn panes(&self) -> usize {
        2
    }
    fn list_len(&self, app: &App) -> usize {
        if app.nav.pane == 1 { app.dash.offers.len() + app.dash.peers.len() } else { app.dash.contacts.len() }
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_payments(f, app, t, area);
    }
}

/// The on-chain message board and private messages: the inbox.
struct Board;
impl ScreenView for Board {
    fn title(&self) -> &'static str {
        "Board"
    }
    fn feature(&self) -> Option<Feature> {
        Some(Feature::Messaging)
    }
    fn panes(&self) -> usize {
        2
    }
    fn list_len(&self, app: &App) -> usize {
        if app.nav.pane == 1 { app.board_message_count() } else { app.board_rows().len() }
    }
    fn on_open(&self, app: &mut App) {
        app.tick_board();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.board_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_board(f, app, t, area);
    }
}

/// Wallets on this computer: switch, create, import.
struct Wallets;
impl ScreenView for Wallets {
    fn title(&self) -> &'static str {
        "Wallets"
    }
    fn list_len(&self, app: &App) -> usize {
        app.cockpit.list.len()
    }
    fn on_open(&self, app: &mut App) {
        app.load_wallets();
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_wallets(f, app, t, area);
    }
}

struct Activity;
impl ScreenView for Activity {
    fn title(&self) -> &'static str {
        "Activity"
    }
    fn list_len(&self, app: &App) -> usize {
        app.activity_rows().len()
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_activity(f, app, t, area);
    }
}

struct Network;
impl ScreenView for Network {
    fn title(&self) -> &'static str {
        "Network"
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, app: &App) -> usize {
        app.dash.networks.len()
    }
    fn on_open(&self, app: &mut App) {
        app.tick_chain_stats();
    }
    fn key(&self, app: &mut App, key: KeyEvent) -> bool {
        app.network_key(key)
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_node(f, app, t, area);
    }
}

struct Settings;
impl ScreenView for Settings {
    fn title(&self) -> &'static str {
        "Settings"
    }
    fn list_len(&self, app: &App) -> usize {
        app.settings_rows().len()
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        ui::draw_settings(f, app, t, area);
    }
}

struct DataSources;
impl ScreenView for DataSources {
    fn title(&self) -> &'static str {
        "Data sources"
    }
    fn pro_only(&self) -> bool {
        true
    }
    fn list_len(&self, _app: &App) -> usize {
        super::app::DATA_SOURCES.len()
    }
    fn draw(&self, f: &mut Frame, app: &App, t: &Theme, area: Rect) {
        views::draw_data_sources(f, app, t, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every screen has an impl, and the enum's facts are the impl's.
    #[test]
    fn every_screen_answers_for_itself() {
        let mut titles = std::collections::HashSet::new();
        for s in Screen::ALL {
            let v = view(s);
            assert!(titles.insert(v.title()), "{} twice", v.title());
            assert!(v.panes() >= 1);
            assert_eq!(s.title(), v.title());
        }
    }
}
