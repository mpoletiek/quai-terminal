use super::*;

/// The receive tab reads " QUAI " in bold on the QUAI color, the same style as the QUAI
/// badge: only the badge itself becomes a logo, and the label keeps its letters.
#[test]
fn a_bold_label_is_not_mistaken_for_a_badge() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("t", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let mut caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Pixels);
    caps.truecolor = true;
    let theme = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-dark", false, false).0;
    let connected = wallet_core::config::AppConfig {
        explorer_lookups: true,
        images: true,
        token_icons: true,
        features: wallet_core::config::Features { messaging: true, trading: true, nfts: true },
        ..Default::default()
    };
    let mut app = App::new(paths, "local".into(), connected, theme, caps, Some(meta));
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::ZERO,
        locked: U256::ZERO,
        nonce: 0,
    }];
    for asset in ["quai", "qi"] {
        let url = wallet_core::media::native_icon(asset).unwrap();
        let wallet_core::media::Source::Inline(bytes) = wallet_core::media::resolve(url).unwrap() else { panic!("inline") };
        let r = std::sync::Arc::new(wallet_core::media::make_rendition(&bytes, wallet_core::media::ICON).unwrap());
        app.eco
            .images
            .insert((url.to_string(), wallet_core::media::ICON), super::super::eco::ImageSlot::Ready(r, std::time::Instant::now()));
    }
    app.modal = Modal::Receive { asset_qi: false, account: 0 };
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let placed = super::super::images::kitty_items(&app);
    let line = |y: u16| -> String {
        let buffer = term.backend().buffer();
        (0..120).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect()
    };
    let tabs = (0..40).find(|y| line(*y).contains("tab switch")).expect("the switch row");
    let inline: Vec<u16> = placed.iter().filter(|p| (p.cols, p.rows, p.y) == (2, 1, tabs)).map(|p| p.x).collect();
    assert_eq!(inline.len(), 2, "one logo per tab, not one per bold QU: {inline:?}");
    assert!(line(tabs).contains("QUAI") && line(tabs).contains("Qi"), "labels keep their letters: {:?}", line(tabs));
}

/// A wallet test app with no onboarding, ready to draw.
#[cfg(test)]
fn drawable_app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("t", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Cells);
    let theme = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-dark", false, false).0;
    let connected = wallet_core::config::AppConfig {
        explorer_lookups: true,
        images: true,
        token_icons: true,
        features: wallet_core::config::Features { messaging: true, trading: true, nfts: true },
        ..Default::default()
    };
    let mut app = App::new(paths, "local".into(), connected, theme, caps, Some(meta));
    app.locked = false;
    app.onboarding = None;
    app.config.motion = Motion::Off;
    (dir, app)
}

/// A transaction waiting to be mined shows one line in the bottom-right corner — never a
/// second row, and never once it is mined.
#[test]
fn pending_transactions_take_one_corner_line() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::appdb::{OpStatus, Operation};
    let (_dir, mut app) = drawable_app();
    let op = |status| Operation {
        id: "a1".into(),
        network: "local".into(),
        kind: "send_quai".into(),
        store: "quai".into(),
        account: "0x00".into(),
        status,
        tx_hash: None,
        asset: "QUAI".into(),
        amount: "1500000000000000000".into(),
        counterparty: String::new(),
        fee: "0".into(),
        detail: serde_json::json!({}),
        created: 0,
        updated: wallet_core::registry::now(),
    };
    let (w, h) = (120u16, 40u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    let row = |term: &Terminal<TestBackend>, y: u16| -> String {
        let buffer = term.backend().buffer();
        (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect()
    };
    app.dash.ops = vec![op(OpStatus::Submitted)];
    term.draw(|f| draw(f, &mut app)).unwrap();
    let last = row(&term, h - 1);
    assert!(last.contains("1.5 QUAI"), "the pill summarises the transaction: {last:?}");
    assert!(last.trim_end().ends_with("0s"), "and how long it has waited: {last:?}");
    assert!(!row(&term, h - 2).contains("1.5 QUAI"), "one line only: {:?}", row(&term, h - 2));
    // Two waiting: still one line, with the rest counted.
    app.dash.ops = vec![op(OpStatus::Submitted), op(OpStatus::Submitted)];
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(row(&term, h - 1).contains("+1"), "{:?}", row(&term, h - 1));
    assert!(!row(&term, h - 2).contains("1.5 QUAI"), "still one line");
    // Mined: the corner is clear again.
    app.dash.ops = vec![op(OpStatus::Confirmed)];
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!row(&term, h - 1).contains("1.5 QUAI"), "{:?}", row(&term, h - 1));
}

/// The flow column lists swaps from every pool with both tokens, and the chart keeps its
/// The board lists what this wallet follows, the people it can write to, and the channels
/// the board itself has — and a typed filter narrows all three.
#[test]
fn the_board_list_finds_channels_it_does_not_follow() {
    use super::super::eco::BoardRow;
    use wallet_core::messages::ChannelSummary;
    let (_dir, mut app) = drawable_app();
    app.config.board_channels = vec!["general".into()];
    app.eco.board.known = vec![
        ChannelSummary { name: "general".into(), messages: 6, last_at: 0, last_block: 100, recent_blocks: vec![100, 99] },
        ChannelSummary { name: "dev-talk".into(), messages: 1, last_at: 0, last_block: 90, recent_blocks: vec![90] },
    ];
    // A channel already followed is not listed twice.
    let rows = app.board_rows();
    assert_eq!(rows.iter().filter(|r| matches!(r, BoardRow::Channel(c) if c == "general")).count(), 1);
    assert!(rows.contains(&BoardRow::Unfollowed("dev-talk".into(), 1)), "{rows:?}");
    // The filter narrows by name.
    app.eco.board.filter = Some("dev".into());
    assert_eq!(app.board_rows(), vec![BoardRow::Unfollowed("dev-talk".into(), 1)]);
    // Following one moves it up into the kept list, still once.
    app.eco.board.filter = None;
    app.follow_channel("dev-talk");
    let rows = app.board_rows();
    assert!(rows.contains(&BoardRow::Channel("dev-talk".into())));
    assert!(!rows.iter().any(|r| matches!(r, BoardRow::Unfollowed(c, _) if c == "dev-talk")), "{rows:?}");
    // A name the contract would refuse is refused here too.
    let before = app.config.board_channels.len();
    app.follow_channel(&"x".repeat(33));
    assert_eq!(app.config.board_channels.len(), before);
}

/// Following a channel means being told what arrived in it: nothing already on the board
/// when the wallet opened, everything after that, and nothing once it has been read.
#[test]
fn a_followed_channel_counts_only_what_arrived_after_you_looked() {
    use wallet_core::messages::ChannelSummary;
    let (_dir, mut app) = drawable_app();
    app.config.board_channels = vec!["general".into()];
    let summary = |blocks: Vec<u64>| ChannelSummary {
        name: "general".into(),
        messages: blocks.len() as u32,
        last_at: 0,
        last_block: blocks.first().copied().unwrap_or(0),
        recent_blocks: blocks,
    };
    // The first scan only records where the board stands.
    app.eco.board.known = vec![summary(vec![100, 99, 98])];
    app.on_data_event(super::super::data::DataEv::BoardChannels(Ok(app.eco.board.known.clone())));
    assert_eq!(app.board_unread("general"), 0, "opening the wallet is not news");
    assert!(app.toasts.is_empty(), "{:?}", app.toasts.iter().map(|t| &t.text).collect::<Vec<_>>());
    // Two arrive while the wallet is elsewhere.
    app.switch(Screen::Home);
    app.on_data_event(super::super::data::DataEv::BoardChannels(Ok(vec![summary(vec![102, 101, 100, 99, 98])])));
    assert_eq!(app.board_unread("general"), 2);
    assert!(app.toasts.iter().any(|t| t.text.contains("2 new messages in #general")), "{:?}", app.toasts.last().map(|t| &t.text));
    // The same scan again says nothing further.
    let said = app.toasts.len();
    app.on_data_event(super::super::data::DataEv::BoardChannels(Ok(vec![summary(vec![102, 101, 100, 99, 98])])));
    assert_eq!(app.toasts.len(), said, "a count that has not moved is not news");
    // The list says how many are waiting, where it would otherwise say how many there are.
    {
        use ratatui::{Terminal, backend::TestBackend};
        app.switch(Screen::Board);
        app.eco.board.filter = None;
        let (w, h) = (120u16, 30u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = {
            let buffer = term.backend().buffer();
            (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect()
        };
        assert!(screen.contains("2 new"), "the channel row counts what is waiting");
    }
    // Reading the channel clears it.
    app.on_data_event(super::super::data::DataEv::Board { channel: "general".into(), result: Ok(Vec::new()) });
    assert_eq!(app.board_unread("general"), 0, "reading a channel is what makes it read");
}

/// The wallets screen lists every wallet on this computer and marks the open one, so two
/// wallets in two terminals can be told apart at a glance.
#[test]
fn the_wallets_screen_marks_the_open_wallet() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = drawable_app();
    let registry = app.registry.clone();
    let other = registry.create_watch("zzsecond", &[("0x000B2E36297c1b133Ca1DCA9881Dc820018f1F79".into(), "Main".into())]).unwrap();
    app.load_wallets();
    assert!(app.wallets.len() >= 2, "the test wallet and the one just made: {}", app.wallets.len());
    app.switch(Screen::Wallets);
    let (w, h) = (120u16, 30u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let rows: Vec<String> = {
        let buffer = term.backend().buffer();
        (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect()).collect()
    };
    // The table rows are the ones naming a wallet kind; the header and tabs are not.
    // Rows inside the panel: the header names the open wallet's kind too, and is not one.
    // The selected wallet's detail, under the list, says watch-only too; it is not a row.
    let listed: Vec<&String> = rows.iter().filter(|r| r.contains("watch-only") && r.contains("││") && !r.contains("cannot sign")).collect();
    assert_eq!(listed.len(), 2, "both wallets are listed: {listed:?}");
    let open = app.meta.as_ref().unwrap().name.clone();
    let marked: Vec<&&String> = listed.iter().filter(|r| r.contains("▸")).collect();
    assert_eq!(marked.len(), 1, "exactly one is open: {listed:?}");
    assert!(!marked[0].contains(&other.name), "and it is not the one just made: {:?}", marked[0]);
    let theirs = listed.iter().find(|r| r.contains(&other.name)).expect("the other wallet is listed");
    assert!(!theirs.contains("▸"), "which is not marked open: {theirs:?}");
    let _ = open;
    // Switching to a wallet that is already open changes nothing and says so.
    let before = app.meta.as_ref().unwrap().id.clone();
    app.switch_wallet(&before);
    assert_eq!(app.meta.as_ref().unwrap().id, before);
    assert!(app.toasts.iter().any(|t| t.text.contains("already on")), "{:?}", app.toasts.last().map(|t| &t.text));
}

/// Tab hands the cursor to the flow column, a row there takes the chart to its pair, and
/// each pane keeps its place while the other has the cursor.
#[test]
fn a_flow_row_takes_the_chart_to_its_pair() {
    use crossterm::event::{KeyCode, KeyEvent};
    let (_dir, mut app) = markets_app();
    app.switch(Screen::Markets);
    let size = (160, 44);
    let key = |app: &mut App, c: KeyCode| app.on_key(KeyEvent::new(c, crossterm::event::KeyModifiers::NONE), size);
    assert_eq!((app.pane, app.markets_pair()), (0, 0), "the pairs list starts with the cursor");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.pane, 1, "tab moves it to the flow");
    // The second row is the WQUAI/QOWBOY pool; taking it moves the chart, not the cursor.
    key(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected, 1);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.markets_pair(), 1, "the chart followed the swap to its pair");
    assert_eq!(app.selected, 1, "the flow kept its own place");
    key(&mut app, KeyCode::Tab);
    assert_eq!((app.pane, app.selected), (0, 1), "and the pairs list came back on that pair");
    // `o` copies the swap's transaction, not the pool's.
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char('o'));
    assert!(app.clipboard.as_ref().is_none_or(|c| c.text.as_str().contains("0x2")), "{:?}", app.clipboard);
}

/// The dust floor hides the small trades, and a swap nobody can price survives it.
#[test]
fn the_dust_floor_hides_small_trades_but_never_unpriced_ones() {
    use crossterm::event::{KeyCode, KeyEvent};
    let (_dir, mut app) = markets_app();
    app.switch(Screen::Markets);
    // QOWBOY is worth a cent, so the 8-QOWBOY swap is 8 cents; the USDT one is $120.
    app.eco.markets = vec![wallet_core::explorer::TokenMarket {
        address: "0x0031".into(),
        symbol: "QOWBOY".into(),
        price_usd: Some(0.01),
        ..wallet_core::explorer::TokenMarket::default()
    }];
    assert_eq!(app.flow_rows().len(), 2, "no floor shows both");
    // The floor is in the action sheet (space m), and `.` steps it while the flow has the focus.
    app.on_key(KeyEvent::new(KeyCode::Char(' '), crossterm::event::KeyModifiers::NONE), (160, 44));
    app.on_key(KeyEvent::new(KeyCode::Char('m'), crossterm::event::KeyModifiers::NONE), (160, 44));
    assert_eq!(app.eco.markets_view.flow_min_usd, 1.0);
    let rows = app.flow_rows();
    assert_eq!(rows.len(), 1, "the 8-cent swap is dust at a dollar");
    assert_eq!(rows[0].pool, "0x0021");
    // Without USDT configured on this network the first swap cannot be priced — and an
    // unpriceable swap is never hidden, because nothing proves it is small.
    app.eco.markets.clear();
    assert_eq!(app.flow_rows().len(), 2, "what cannot be judged is not dropped");
}

/// own width beside it.
/// A wallet showing two pools and one swap through each: USDT → WQUAI, and QOWBOY → WQUAI.
#[cfg(test)]
fn markets_app() -> (tempfile::TempDir, App) {
    use wallet_core::markets::{DexOverview, DexSwap, Pool, PoolToken};
    let (dir, mut app) = drawable_app();
    let token = |address: &str, symbol: &str, decimals| PoolToken { address: address.into(), symbol: symbol.into(), decimals };
    let usdt = token("0x0049", "USDT", 6);
    let wquai = token("0x006c", "WQUAI", 18);
    let qowboy = token("0x0031", "QOWBOY", 18);
    let pools = vec![
        Pool { address: "0x0021".into(), token0: usdt.clone(), token1: wquai.clone(), ..Pool::default() },
        Pool { address: "0x0022".into(), token0: wquai.clone(), token1: qowboy.clone(), ..Pool::default() },
    ];
    let swap = |pool: &str, index, token_in: &PoolToken, amount_in, token_out: &PoolToken, amount_out| DexSwap {
        at: wallet_core::registry::now() - index,
        timed: true,
        block: 100 - index,
        tx: format!("0x{index}"),
        index,
        pool: pool.into(),
        token_in: token_in.clone(),
        token_out: token_out.clone(),
        amount_in,
        amount_out,
        trader: "0x00ab".into(),
    };
    app.eco.markets_view.pools = Some(Ok((pools, DexOverview::default())));
    app.eco.markets_view.flow = vec![swap("0x0021", 1, &usdt, 120.0, &wquai, 11_800.0), swap("0x0022", 2, &qowboy, 8.0, &wquai, 1200.0)];
    (dir, app)
}

/// A pairs list that a ceiling shortened says so in its title, in words and with a mark, and
/// says nothing when the list is whole. At a narrow width the title is clipped by its panel
/// rather than pushing the layout about.
#[test]
fn a_partial_market_list_admits_it() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::markets::Omitted;
    let render = |app: &mut App, w: u16, h: u16| {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buffer = term.backend().buffer();
        (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect::<Vec<_>>()
    };
    let (_dir, mut app) = markets_app();
    app.switch(Screen::Markets);
    let whole = render(&mut app, 160, 44).join("\n");
    assert!(!whole.contains("⚠") && !whole.contains("newest"), "a whole list claims nothing");
    if let Some(Ok((_, overview))) = app.eco.markets_view.pools.as_mut() {
        overview.omitted.push(Omitted { source: "launch AMM factory".into(), read: 600, total: 812 });
    }
    let wide = render(&mut app, 160, 44);
    let title = wide.iter().find(|l| l.contains("pairs")).expect("the pairs panel is drawn");
    // The warning comes first and survives the panel's width whole: a mark, the number, the word.
    assert!(title.contains("pairs · ⚠ 212 not listed"), "{title}");
    // Two sources add up rather than crowding each other out.
    if let Some(Ok((_, overview))) = app.eco.markets_view.pools.as_mut() {
        overview.omitted.push(Omitted { source: "Quainance factory".into(), read: 600, total: 610 });
    }
    let both = render(&mut app, 160, 44);
    assert!(both.iter().any(|l| l.contains("⚠ 222 not listed")), "both sources are counted");
    // Narrow: the pairs list stacks above the chart and its title is clipped, never wrapped.
    let narrow = render(&mut app, 90, 30);
    assert!(narrow.iter().all(|l| l.chars().count() == 90), "every row stays the terminal's width");
    assert!(narrow.iter().any(|l| l.contains("pairs")), "the panel still draws");
}

/// A token on its bonding curve reads as a market: marked in the list, and its header says how
/// far it has raised and how to buy it, where a pool shows its TVL.
#[test]
fn a_bonding_curve_market_shows_its_progress() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::markets::{CurveMark, Pool, PoolToken, Venue};
    let (_dir, mut app) = markets_app();
    if let Some(Ok((pools, _))) = app.eco.markets_view.pools.as_mut() {
        pools.push(Pool {
            address: "0x004ce1".into(),
            token0: PoolToken { address: "0x0016c3".into(), symbol: "CHEEZ".into(), decimals: 18 },
            token1: PoolToken { address: "0x006c".into(), symbol: "WQUAI".into(), decimals: 18 },
            venue: Venue::Curve,
            curve: Some(CurveMark {
                venue_kind: Some(wallet_core::capabilities::Family::QuainanceCurve),
                price_basis: Default::default(),
                price_quai: Some(0.0000744),
                raised_quai: 17_131.9,
                target_quai: Some(25_000.0),
                progress_bps: Some(6_852),
                launchpad: None,
                locked_quai: None,
            }),
            ..Pool::default()
        });
    }
    app.switch(Screen::Markets);
    app.selected = 2;
    let (w, h) = (160u16, 44u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = {
        let buffer = term.backend().buffer();
        (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect()
    };
    assert!(screen.contains("CHEEZ/"), "the curve is listed");
    assert!(screen.contains("68%"), "with its progress where a pool shows TVL");
    assert!(screen.contains("bonding curve"), "the chart names the venue");
    assert!(screen.contains("t buy · S sell"), "and how to trade it");
    assert!(screen.contains("raised 17,132 of 25,000 QUAI (68%)"), "{screen}");
}

/// A bonded Hartii curve trades against a pool its graduation seeded and locked. Its depth is
/// that pool, not the "100%" every sold-out curve would read, and its price is the pool's ratio.
#[test]
fn a_bonded_curve_shows_its_locked_depth() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::markets::{CurveMark, Pool, PoolToken, PriceBasis, Venue};
    let (_dir, mut app) = markets_app();
    if let Some(Ok((pools, _))) = app.eco.markets_view.pools.as_mut() {
        pools.push(Pool {
            address: "0x004bc4".into(),
            token0: PoolToken { address: "0x003518".into(), symbol: "QAXE".into(), decimals: 18 },
            token1: PoolToken { address: "0x006c".into(), symbol: "WQUAI".into(), decimals: 18 },
            venue: Venue::Curve,
            curve: Some(CurveMark {
                venue_kind: Some(wallet_core::capabilities::Family::HartiiCurve),
                price_basis: PriceBasis::ReserveSpot,
                price_quai: Some(0.0040174),
                raised_quai: 0.0,
                target_quai: None,
                progress_bps: Some(10_000),
                launchpad: Some("HartiiLabs".into()),
                locked_quai: Some(190_560.68),
            }),
            ..Pool::default()
        });
    }
    app.switch(Screen::Markets);
    app.selected = 2;
    let (w, h) = (160u16, 44u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = {
        let buffer = term.backend().buffer();
        (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect()
    };
    assert!(screen.contains("QAXE/"), "the curve is listed");
    assert!(!screen.contains("100%"), "a bonded curve's depth is not its sell-out");
    assert!(screen.contains("locked 190,561 QUAI"), "{screen}");
    assert!(screen.contains("no LP token"), "and that depth cannot leave");
    assert!(screen.contains("reserve spot (before fee)"), "the price says what it measures");
}

#[test]
fn the_flow_column_shows_swaps_from_every_pool() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = markets_app();
    app.switch(Screen::Markets);
    let (w, h) = (160u16, 44u16);
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let screen: String = {
        let buffer = term.backend().buffer();
        (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect()
    };
    assert!(screen.contains("flow · all pools"), "the column is on screen");
    for symbol in ["USDT", "QOWBOY"] {
        assert!(screen.contains(symbol), "{symbol} is named in the flow");
    }
    assert!(screen.contains("USDT→"), "each row names what went in and what came out");
    assert!(screen.contains("flow · all pools"), "and the column says what it is");
    // The chart keeps its own width: the pair header sits beside the column, not under it.
    assert!(screen.contains("pairs · stale/partial source"), "the pairs list is still there above the flow");
}

#[test]
fn big_digits_have_equal_rows() {
    let rows = big_digits("179,071");
    assert_eq!(rows[0].chars().count(), rows[2].chars().count());
    assert!(rows[0].chars().count() > 20);
}

#[test]
fn activity_names_contacts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("t", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Cells);
    let theme = super::super::theme::Theme::terminal(false);
    let connected = wallet_core::config::AppConfig {
        explorer_lookups: true,
        images: true,
        token_icons: true,
        features: wallet_core::config::Features { messaging: true, trading: true, nfts: true },
        ..Default::default()
    };
    let mut app = App::new(paths, "local".into(), connected, theme, caps, Some(meta));
    let code = "PM8TJcZEnoV5gpCTuZiGKMUKrrPTr4K5D1SWDD";
    app.dash.contacts = vec![
        wallet_core::appdb::Contact { id: 1, name: "bob".into(), address: None, payment_code: Some(code.into()), note: String::new() },
        wallet_core::appdb::Contact {
            id: 2,
            name: "alice".into(),
            address: Some("0x00AbCdef00000000000000000000000000000001".into()),
            payment_code: None,
            note: String::new(),
        },
    ];
    let op = |kind: &str, counterparty: &str, detail: serde_json::Value| Operation {
        id: "x".into(),
        network: "local".into(),
        kind: kind.into(),
        store: "quai".into(),
        account: "0x00".into(),
        status: OpStatus::Confirmed,
        tx_hash: None,
        asset: "QUAI".into(),
        amount: "1000000000000000000".into(),
        counterparty: counterparty.into(),
        fee: "0".into(),
        detail,
        created: 0,
        updated: 0,
    };
    // Addresses match case-insensitively; Qi sends match through the peer code.
    assert_eq!(op_contact(&app, &op("send_quai", "0x00abcdef00000000000000000000000000000001", serde_json::json!({}))), Some("alice"));
    assert_eq!(op_contact(&app, &op("send_qi", "bob (PM8TJcZE…)", serde_json::json!({"peer": code}))), Some("bob"));
    assert_eq!(op_contact(&app, &op("send_quai", "0x0000000000000000000000000000000000000009", serde_json::json!({}))), None);
    let theme = super::super::theme::Theme::terminal(false);
    assert!(
        op_row_parts(&app, &theme, &op("send_quai", "0x00ABCDEF00000000000000000000000000000001", serde_json::json!({})))
            .1
            .ends_with("→ alice")
    );

    let received = |detail: serde_json::Value| Activity {
        network: "local".into(),
        key: "k".into(),
        direction: "in".into(),
        asset: "QI".into(),
        amount: "1000".into(),
        address: "0x00F4945eAC522b7C8D2FA80d569aA2854dbb804B".into(),
        tx_hash: None,
        block: None,
        detail,
        observed: 0,
    };
    assert_eq!(activity_contact(&app, &received(serde_json::json!({"peer": code}))), Some("bob"));
    // Rows recorded before the full code was stored.
    let legacy = serde_json::json!({"origin": format!("payment from {}", short_code(code))});
    assert_eq!(activity_contact(&app, &received(legacy)), Some("bob"));
    assert_eq!(activity_contact(&app, &received(serde_json::json!({"origin": "receive #3"}))), None);
}

#[test]
fn scaled_sparkline_right_aligns() {
    assert_eq!(scaled(&[100, 101, 102], 5), vec![0, 0, 1, 2, 3]);
    assert_eq!(scaled(&[], 3), vec![0, 0, 0]);
}

/// Every modal the golden test draws, over a populated app.
pub(crate) fn modals(app: &App) -> Vec<Modal> {
    use super::super::app::{ConfirmAction, Picker, ReviewState};

    vec![
        Modal::None,
        Modal::Help,
        Modal::Palette { query: "con".into(), selected: 1 },
        Modal::Receive { asset_qi: false, account: 0 },
        Modal::Notifications,
        Modal::Wallets { selected: 0 },
        Modal::Confirm { title: "Quit".into(), body: "Leave?".into(), action: ConfirmAction::Quit },
        Modal::Themes(Picker::new(app)),
        Modal::Effects(super::super::app::Gallery::new("decrypt")),
        Modal::Review(ReviewState {
            review: wallet_core::tx::Review {
                op_id: "x".into(),
                kind: "send_quai".into(),
                title: "Send QUAI".into(),
                network: "Local dev".into(),
                from: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a (Account 1)".into(),
                to: "0x00F41a2B3c4D5e6F7a8B9c0D1e2F3a4B5c6D804B".into(),
                asset: "QUAI".into(),
                amount: "1 QUAI".into(),
                amount_base: "1".into(),
                max_fee: "0.1 QUAI".into(),
                fee_bps: Some(1000),
                fields: vec![],
                coins: vec![],
                warnings: vec!["a warning".into()],
                visuals: vec![
                    wallet_core::tx::ReviewVisual { role: "pay".into(), symbol: "QUAI".into(), contract: "quai".into(), token_id: None },
                    wallet_core::tx::ReviewVisual {
                        role: "receive".into(),
                        symbol: "USDT".into(),
                        contract: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(),
                        token_id: None,
                    },
                ],
                fee_over_policy: true,
                changes: wallet_core::tx::balance_changes(
                    "send_quai",
                    "QUAI",
                    U256::from(10u64).pow(U256::from(18u8)),
                    18,
                    U256::from(10u64).pow(U256::from(18u8)),
                    (U256::from(10u64).pow(U256::from(17u8)), "QUAI", 18),
                    &serde_json::Value::Null,
                ),
            },
            scroll: 0,
            content_lines: 1,
            viewport: 1,
            approve_focused: false,
            opened: std::time::Instant::now(),
        }),
        Modal::Review(ReviewState {
            review: wallet_core::tx::Review {
                op_id: "y".into(),
                kind: "nft_buy".into(),
                title: "Buy Quai Pepe #212".into(),
                network: "Quai Mainnet".into(),
                from: "0x00 (Account 1)".into(),
                to: "0x0012".into(),
                asset: "QUAI".into(),
                amount: "1000 QUAI".into(),
                amount_base: "1".into(),
                max_fee: "0.1 QUAI".into(),
                fee_bps: None,
                fields: vec![],
                coins: vec![],
                warnings: vec![],
                visuals: vec![wallet_core::tx::ReviewVisual {
                    role: "nft".into(),
                    symbol: "Quai Pepe #212".into(),
                    contract: "0x004d92fd198c21af21016f4b119b8b851b5aeaa4".into(),
                    token_id: Some("212".into()),
                }],
                fee_over_policy: false,
                changes: wallet_core::tx::balance_changes(
                    "nft_buy",
                    "QUAI",
                    U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u8)),
                    18,
                    U256::from(1000u64) * U256::from(10u64).pow(U256::from(18u8)),
                    (U256::from(10u64).pow(U256::from(17u8)), "QUAI", 18),
                    &serde_json::json!({"name": "Quai Pepe #212", "token_id": "212"}),
                ),
            },
            scroll: 0,
            content_lines: 1,
            viewport: 1,
            approve_focused: false,
            opened: std::time::Instant::now(),
        }),
    ]
}

/// The populated app every screen test draws: a watch-only wallet with holdings, activity,
/// markets, NFTs, launches and pools, on a frozen clock. The directory must outlive the app.
pub(crate) fn populated_app() -> (tempfile::TempDir, App) {
    // Fixtures are dated from now and candles bucket on the clock, so the golden screens are
    // drawn at one fixed moment (2026-09-18, part-way through an hour).
    wallet_core::registry::freeze_clock(Some(1_789_705_234));
    use wallet_core::appdb::{Contact, Notice};
    use wallet_core::network::NodeHealth;
    use wallet_core::session::{AccountBalance, CoinView, QiBalanceView, QiSummary};
    use wallet_core::track::LockItem;

    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("preview", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let mut caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Cells);
    // The same glyphs whichever terminal runs the tests (goldens are compared in CI too).
    caps.drawn_blocks = false;
    let theme = super::super::theme::Theme::terminal(false);
    let connected = wallet_core::config::AppConfig {
        explorer_lookups: true,
        images: true,
        token_icons: true,
        features: wallet_core::config::Features { messaging: true, trading: true, nfts: true },
        ..Default::default()
    };
    let mut app = App::new(paths, "local".into(), connected, theme, caps, Some(meta));
    app.config.motion = Motion::Off;
    let d = &mut app.dash;
    d.network_id = "local".into();
    d.network_name = "Local dev".into();
    d.refreshed_at = wallet_core::registry::now();
    d.health = Some(NodeHealth {
        network: "local".into(),
        chain_id: "1337".into(),
        genesis: "0xff".into(),
        identity_ok: true,
        height: 1_234_567,
        head_hash: "0x00".into(),
        head_age_secs: Some(3),
        gas_price: "1000000000".into(),
        client_version: None,
        latency_ms: 12,
        order: Some(0),
    });
    d.latency_history = vec![10, 12, 9, 30, 11];
    d.height_history = vec![1, 3, 4, 8];
    d.accounts = vec![AccountBalance {
        address: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        label: "Account 1".into(),
        hd_index: Some(0),
        balance: U256::from(179_071_753_371_473_263_599_029u128),
        locked: U256::from(5u64),
        nonce: 11,
    }];
    d.qi = Some(QiSummary {
        balance: QiBalanceView {
            total: U256::from(250_000u64),
            spendable: U256::from(240_000u64),
            reserved: U256::from(5_000u64),
            locked: U256::from(5_000u64),
            expired: U256::ZERO,
        },
        checkpoint_height: Some(1_234_560),
        coins: (0..40)
            .map(|i| CoinView {
                outpoint: format!("0x{i:064x}:0"),
                address: "0x00F4945eAC522b7C8D2FA80d569aA2854dbb804B".into(),
                qits: [1u64, 10, 100, 1000, 10000][i % 5],
                denomination: [0u8, 2, 4, 6, 8][i % 5],
                unlock_height: U256::from(if i % 7 == 0 { 2_000_000u64 } else { 0 }),
                reserved: i % 11 == 0,
                origin: "payment from PM8TJ…".into(),
                peer: None,
                label: None,
            })
            .collect(),
    });
    d.locks = vec![LockItem {
        source: "QUAI→Qi conversion".into(),
        asset: "QI".into(),
        amount: "2.4".into(),
        unlock_height: Some(1_300_000),
        blocks_remaining: Some(65_433),
        eta_secs: Some(327_165),
        unlocked: false,
    }];
    d.contacts = vec![Contact {
        id: 1,
        name: "bob".into(),
        address: None,
        payment_code: Some("PM8TJcZEnoV5gpCTuZiGKMUKrrPTr4K5D1SWDD".into()),
        note: String::new(),
    }];
    d.notifications = vec![Notice {
        id: 1,
        at: 1_789_705_234 - 60,
        level: "success".into(),
        title: "Confirmed".into(),
        body: "send of 1 QUAI".into(),
        read: false,
    }];

    // Ecosystem fixtures: portfolio, images, NFTs, listings, collections, a quote and an ask.
    {
        use std::sync::Arc;
        use wallet_core::explorer::{Collection, NftItem, TokenKind};
        use wallet_core::market::{Ask, AskCheck, Listing, OwnedNft};
        use wallet_core::portfolio::{AssetKey, AssetRow, Portfolio, PriceKind, Trust, ValuePoint};
        let row = |key: AssetKey, symbol: &str, bal: &str, dec: u8, price: Option<f64>, trust: Trust, icon: Option<&str>| AssetRow {
            key,
            symbol: symbol.into(),
            name: format!("{symbol} token"),
            balance: bal.into(),
            decimals: dec,
            exact: symbol != "BOSS",
            price_usd: price,
            price_kind: if symbol == "Qi" {
                PriceKind::Protocol
            } else if price.is_some() {
                PriceKind::Market
            } else {
                PriceKind::None
            },
            price_source: "mexc via explorer.qu.ai".into(),
            price_at: wallet_core::registry::now() - 60,
            value_usd: price.map(|p| p * 100.0),
            allocation: 0.25,
            change_24h: Some(2.1),
            icon_url: icon.map(str::to_string),
            trust,
            holders: Some(230),
        };
        let icon = "data:image/png;base64,icon";
        let thumb = "data:image/png;base64,thumb";
        let wqi = "0x002b2596ecf05c93a31ff916e8b456df6c77c750".to_string();
        app.eco.portfolio = Some(Portfolio {
            network: "local".into(),
            rows: vec![
                row(AssetKey::Quai, "QUAI", "90412200000000000000000", 18, Some(0.00881), Trust::Verified, None),
                row(AssetKey::Qi, "Qi", "256610", 3, Some(0.9), Trust::Verified, None),
                row(AssetKey::Token(wqi.clone()), "WQI", "171400000000000000000", 18, Some(1.0492), Trust::Verified, Some(icon)),
                row(
                    AssetKey::Token("0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into()),
                    "USDT",
                    "50480000",
                    6,
                    Some(1.01),
                    Trust::Verified,
                    None,
                ),
                row(
                    AssetKey::Token("0x004c7926967b899ea69e871a366a5b344660f7eb".into()),
                    "BOSS",
                    "1200000000000000000000000",
                    18,
                    None,
                    Trust::Unverified,
                    None,
                ),
            ],
            total_usd: 1284.52,
            unpriced: 1,
            // Seven days, every six hours, ending now.
            history: (0..28u64).map(|i| ValuePoint { at: 1_789_705_234 - (27 - i) * 21_600, usd: 1200.0 + i as f64 * 3.0 }).collect(),
            change_7d: Some(4.1),
            nfts: wallet_core::portfolio::NftSummary { items: 3, collections: 3 },
            prices: Some(wallet_core::explorer::PriceBoard {
                quai_usd: Some(0.00881),
                quai_source: "mexc".into(),
                qi_usd: Some(0.9),
                qi_source: "derived:protocol-rate".into(),
                taken_at: 1_789_705_234 - 180,
                observed_at: 1,
            }),
            sources: vec!["explorer.qu.ai".into()],
            notices: vec!["token prices unavailable: explorer.qu.ai: timed out".into()],
            stale: true,
            observed_at: 1,
        });
        let rendition = |w: u32, h: u32| {
            Arc::new(wallet_core::media::make_rendition(&wallet_core::media::fixture_png(w, h, (200, 40, 90)), w.max(h)).unwrap())
        };
        app.eco.images.insert(
            (icon.to_string(), wallet_core::media::ICON),
            super::super::eco::ImageSlot::Ready(rendition(32, 32), std::time::Instant::now()),
        );
        app.eco.images.insert(
            (thumb.to_string(), wallet_core::media::THUMB),
            super::super::eco::ImageSlot::Ready(rendition(256, 180), std::time::Instant::now()),
        );
        let pepe = "0x00469e66615ec9db6b2eb4ad526c735d2887410d".to_string();
        let item = NftItem {
            contract: pepe.clone(),
            token_id: "212".into(),
            kind: Some(TokenKind::Erc721),
            name: "Quai Pepe #212".into(),
            collection: "Quai Pepes".into(),
            description: "A pepe.".into(),
            image: Some(thumb.into()),
            traits: vec![("background".into(), "blue".into()), ("hat".into(), "crown".into())],
            owner: Some("0x00aa".into()),
            quantity: "1".into(),
        };
        app.eco.nfts = Some(Ok(vec![
            OwnedNft { item: item.clone(), owner: "0x00aa".into(), kind: TokenKind::Erc721, quantity: "1".into(), verified: true },
            OwnedNft {
                item: NftItem { token_id: "7".into(), image: None, name: "Miner #17".into(), ..item.clone() },
                owner: "0x00aa".into(),
                kind: TokenKind::Erc1155,
                quantity: "3".into(),
                verified: true,
            },
        ]));
        let listing = Listing {
            contract: pepe.clone(),
            token_id: "212".into(),
            seller: "0x00bb".into(),
            price: "1000000000000000000000".into(),
            currency: "0x0000000000000000000000000000000000000000".into(),
            protocol: "zora".into(),
            quantity: "1".into(),
            // Listed four hours before the frozen clock.
            created_at: 1_789_705_234 - 4 * 3600,
            name: Some("Quai Pepe #212".into()),
            image: None,
        };
        let seaport = Listing { protocol: "seaport".into(), token_id: "9".into(), ..listing.clone() };
        app.eco.listings.insert(None, Ok(vec![listing.clone(), seaport]));
        app.eco.listings.insert(Some(pepe.clone()), Ok(vec![listing]));
        app.eco.collections = Some(Ok(vec![Collection {
            address: pepe.clone(),
            name: "Quai Pepes".into(),
            symbol: "PEPE".into(),
            kind: Some(TokenKind::Erc721),
            holders: Some(900),
            preview: Some(thumb.into()),
            floor_quai: Some(1000.0),
            floor_usd: Some(8.81),
        }]));
        app.eco.collection_items.insert(pepe.clone(), Ok(vec![item.clone()]));
        app.eco.nft_meta.insert((pepe.clone(), "212".into()), Ok(item));
        app.eco.asks.insert(
            (pepe.clone(), "212".into()),
            Ok(AskCheck {
                ask: Some(Ask {
                    seller: "0x00bb".into(),
                    funds_recipient: "0x00bb".into(),
                    currency: "0x0".into(),
                    finders_fee_bps: 0,
                    price: "1000000000000000000000".into(),
                }),
                owner: Some("0x00bb".into()),
                seller_owns: true,
                seller_module_approved: true,
                seller_helper_approved: true,
                buyer_module_approval_needed: true,
                buyer_token_approval_needed: false,
                valid: true,
                problems: vec![],
            }),
        );
        app.eco.swap.amount = "50".into();
        app.eco.swap.from = wallet_core::swap::SwapAsset::Token { address: wqi.clone(), symbol: "WQI".into(), decimals: 18 };
        app.eco.swap.to = Some(wallet_core::swap::SwapAsset::Token {
            address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(),
            symbol: "USDT".into(),
            decimals: 6,
        });
        app.eco.swap.quote_key = 7;
        app.eco.swap.requested_key = 7;
        app.eco.swap.requested_input = app.swap_input_key();
        app.eco.swap.quote = Some(Ok(wallet_core::swap::SwapQuote {
            from: app.eco.swap.from.clone(),
            to: app.eco.swap.to.clone().unwrap(),
            amount_in: "50000000000000000000".into(),
            amount_out: "51940000".into(),
            minimum_out: "51680000".into(),
            slippage_bps: 50,
            path: vec![wqi.clone(), "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into()],
            route: vec!["WQI".into(), "USDT".into()],
            pools: vec![wallet_core::swap::PoolHop {
                pair: "0x0021f5cc862ebb0252ba209266f2fabbc7592e83".into(),
                reserve_in: "1".into(),
                reserve_out: "1".into(),
                tvl_usd: Some(3261.58),
            }],
            impact_bps: 240,
            fee_bps: 30,
            router: "0x000d6795e06eA4F460CA9572a51741342156305A".into(),
            allowance: Some("0".into()),
            approval_needed: true,
            balance: Some("171400000000000000000".into()),
            insufficient: false,
            warnings: vec!["price impact is 2.40% — the pools are thin for this amount".into()],
            observed_at: 1,
            liquidity_at: Some(1),
            legs: vec![],
        }));
        app.eco.lockups = Some(Ok(0));
        app.eco.pnl = Some(Ok(sample_pnl()));
        app.eco.pnl_at = Some(std::time::Instant::now());
        // Markets: one pool with a day of swaps and syncs.
        {
            use wallet_core::markets::{DexOverview, Pool, PoolEvent, PoolToken};
            let pool = Pool {
                address: "0x0021f5cc862ebb0252ba209266f2fabbc7592e83".into(),
                token0: PoolToken { address: "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".into(), symbol: "USDT".into(), decimals: 6 },
                token1: PoolToken { address: "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb".into(), symbol: "WQUAI".into(), decimals: 18 },
                reserve0: 1665.4,
                reserve1: 182_985.5,
                tvl_usd: Some(3330.9),
                volume_24h_usd: Some(312.6),
                ..Default::default()
            };
            let now = wallet_core::registry::now();
            let mut events = Vec::new();
            for k in 0..30u64 {
                let at = now - k * 2400;
                let usdt = 1600.0 + (k % 7) as f64 * 9.0;
                events.push(PoolEvent::Sync {
                    at,
                    block: at,
                    tx: format!("0x{k:064x}"),
                    index: 1,
                    reserve0: wallet_core::sdk::U256::from((usdt * 1e6) as u128),
                    reserve1: wallet_core::sdk::U256::from(182_985u128) * wallet_core::sdk::U256::from(10u128.pow(18)),
                });
                events.push(PoolEvent::Swap {
                    at,
                    block: at,
                    tx: format!("0x{k:064x}"),
                    index: 0,
                    amount0_in: wallet_core::sdk::U256::from(10_000_000u64 + k * 100_000),
                    amount1_in: wallet_core::sdk::U256::ZERO,
                    amount0_out: wallet_core::sdk::U256::ZERO,
                    amount1_out: wallet_core::sdk::U256::from(1_100u128) * wallet_core::sdk::U256::from(10u128.pow(18)),
                    to: "0x002360bc8e2a359be7335b06de43f1c7f040f15a".into(),
                });
            }
            app.eco.markets_view.events.insert(pool.address.clone(), Ok(events));
            // A graduated launch on the launch AMM, and one still on its bonding curve.
            let wquai = || PoolToken { address: "0x006c3e2aaae5db1bcd11a1a097ce572312eaddbb".into(), symbol: "WQUAI".into(), decimals: 18 };
            let graduated = Pool {
                address: "0x001dac18c8702f07d18b1bcdca45e85c6b8364a6".into(),
                token0: PoolToken { address: "0x0048848ca70ea1560577b4725a84b23b6bc589e2".into(), symbol: "QOGE".into(), decimals: 18 },
                token1: wquai(),
                reserve0: 153_868_445.9,
                reserve1: 62_501.1,
                tvl_usd: Some(997.6),
                venue: wallet_core::markets::Venue::LaunchAmm,
                ..Default::default()
            };
            let bonding = Pool {
                address: "0x004ce1cbb33cad511b79d52c6e1118ce4eb60db3".into(),
                token0: PoolToken { address: "0x0016c3221b6a1707427d660945cd284a9be58cec".into(), symbol: "CHEEZ".into(), decimals: 18 },
                token1: wquai(),
                venue: wallet_core::markets::Venue::Curve,
                curve: Some(wallet_core::markets::CurveMark {
                    venue_kind: Some(wallet_core::capabilities::Family::QuainanceCurve),
                    price_basis: Default::default(),
                    price_quai: Some(0.0000744),
                    raised_quai: 17_131.9,
                    target_quai: Some(25_000.0),
                    progress_bps: Some(6_852),
                    launchpad: None,
                    locked_quai: None,
                }),
                ..Default::default()
            };
            app.eco.markets_view.pools = Some(Ok((
                vec![pool, graduated, bonding],
                DexOverview {
                    tvl_usd: Some(11_900.0),
                    volume_24h_usd: Some(2870.0),
                    source: "explorer.qu.ai".into(),
                    ..DexOverview::default()
                },
            )));
        }
        // A staked position and an unstaked one, so Trade › Pools renders both states.
        {
            use wallet_core::gauge::{GaugePool, GaugeView, RewardStream};
            use wallet_core::liquidity::LpPosition;
            use wallet_core::markets::PoolToken;
            let tok = |a: &str, sym: &str, d: u8| PoolToken { address: a.into(), symbol: sym.into(), decimals: d };
            let e18 = |n: u128| U256::from(n) * U256::from(10u128.pow(18));
            app.eco.pools_view.positions = Some(Ok(vec![
                LpPosition {
                    gauge_address: None,
                    pair: "0x0021".into(),
                    token0: tok("0x002b", "WQI", 18),
                    token1: tok("0x006c", "WQUAI", 18),
                    lp_wallet: e18(2),
                    lp_staked: e18(2),
                    lp_total: e18(1_000),
                    amount0: e18(18),
                    amount1: e18(2_214),
                    usd: Some(284.10),
                    pid: Some(0),
                    gauge: Some(wallet_core::gauge::GaugeKind::Core),
                },
                LpPosition {
                    gauge_address: None,
                    pair: "0x0022".into(),
                    token0: tok("0x0049", "USDT", 6),
                    token1: tok("0x006c", "WQUAI", 18),
                    lp_wallet: e18(1),
                    lp_staked: e18(1),
                    lp_total: e18(900),
                    amount0: U256::from(51_000_000u64),
                    amount1: e18(5_920),
                    usd: Some(151.22),
                    pid: Some(0),
                    gauge: Some(wallet_core::gauge::GaugeKind::Zone),
                },
            ]));
            app.eco.pools_view.gauge = Some(GaugeView {
                address: "0x0051".into(),
                pools: vec![GaugePool {
                    pid: 0,
                    lp_token: "0x0021".into(),
                    total_staked: e18(7_574),
                    lp_supply: e18(360_684),
                    staked: e18(2),
                    rewards: vec![RewardStream {
                        token: tok("0x006c", "WQUAI", 18),
                        rate: U256::from_str_radix("1286008230452674897119341563786008", 10).unwrap_or_default(),
                        period_finish: wallet_core::registry::now() + 55 * 86_400,
                        earned: e18(12),
                    }],
                }],
                ..GaugeView::default()
            });
            // A launch-zone campaign on the second position, which the core gauge has no
            // pool for: the two are rendered as separate things. The preview focuses it, so
            // the campaign's lines are in the capture.
            app.eco.pools_view.selected = 1;
            app.eco.pools_view.zone = Some(wallet_core::zone::ZoneView {
                gauges: vec!["0x004a".into()],
                pools: vec![wallet_core::zone::ZonePool {
                    gauge: "0x004a".into(),
                    factory: "0x0018".into(),
                    pid: 0,
                    lp_token: "0x0022".into(),
                    launch_token: "0x0003".into(),
                    quote_asset: "0x002b".into(),
                    total_staked: e18(48_623),
                    activation_threshold: e18(270),
                    lp_supply: e18(385_022),
                    staked: e18(1),
                    campaign: wallet_core::zone::Campaign {
                        total_reward: e18(25_000_000),
                        emitted_reward: e18(6_704_606),
                        duration: 7_257_600,
                        activation_deadline: wallet_core::registry::now() + 20 * 86_400,
                        start: wallet_core::registry::now() - 22 * 86_400,
                        finish: wallet_core::registry::now() + 62 * 86_400,
                        activated: true,
                        expired: false,
                    },
                    rewards: vec![wallet_core::zone::ZoneReward {
                        token: tok("0x0003", "SMOL", 18),
                        rate: U256::from(3_444_664_902_998_236_331u128),
                        period_finish: wallet_core::registry::now() + 62 * 86_400,
                        remaining: e18(18_295_393),
                        earned: e18(1_204),
                    }],
                }],
                ..wallet_core::zone::ZoneView::default()
            });
        }
        app.eco.test =
            Some(vec![("explorer.qu.ai".into(), Ok("QUAI $0.0088".into()), 80), ("node".into(), Err("timed out".into()), 12000)]);
        app.dash.activity.push(wallet_core::appdb::Activity {
                network: "local".into(),
                key: "tt:0x1:0:in".into(),
                direction: "in".into(),
                asset: "QM".into(),
                amount: "1".into(),
                address: "0x00".into(),
                tx_hash: Some("0x1".into()),
                block: Some(1),
                detail: serde_json::json!({"source": "explorer", "standard": "ERC-721", "token_id": "12", "name": "Quai Miners", "counterparty": "0x00cc"}),
                // Two hours before the frozen clock.
                observed: 1_789_705_234 - 7200,
            });
    }
    // Mainnet profile so swap and marketplace views render their full cards.
    app.network_id = "mainnet".into();
    (dir, app)
}

/// Renders every screen and modal across themes and sizes: catches layout panics/overflow.
#[test]
fn every_screen_renders_at_every_size() {
    use super::super::app::Picker;
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = populated_app();
    let details = vec![
        super::super::app::Detail::Asset("quai".into()),
        super::super::app::Detail::Asset("0x002b2596ecf05c93a31ff916e8b456df6c77c750".into()),
        super::super::app::Detail::Nft("0x00469e66615ec9db6b2eb4ad526c735d2887410d".into(), "212".into()),
        super::super::app::Detail::Collection("0x00469e66615ec9db6b2eb4ad526c735d2887410d".into()),
        super::super::app::Detail::Activity("act:tt:0x1:0:in".into()),
        super::super::app::Detail::Activity("op:missing".into()),
    ];
    // The launch zone: one on its curve, one graduated, one pooled.
    {
        use wallet_core::launches::{Launch, Phase};
        let launch = |symbol: &str, phase, progress, price| Launch {
            token: format!("0x00{:0>38}", symbol.len()),
            symbol: symbol.into(),
            name: format!("{symbol} token"),
            phase,
            venue: "Quainance bonding curve".into(),
            progress_bps: progress,
            price_quai: price,
            raised_quai: 12_837.7,
            target_quai: Some(25_000.0),
            buys: 49,
            sells: 15,
            created_at: 1_789_636_189,
            ..Default::default()
        };
        let cheez = Launch {
            curve: Some("0x004ce1cbb33cad511b79d52c6e1118ce4eb60db3".into()),
            ..launch("CHEEZ", Phase::Bonding, Some(5135), Some(0.0000517))
        };
        // A virtual constant-product curve as steep as NOAH's (the price rises about 11x to graduation).
        let sold: Vec<f64> = (0..=48)
            .map(|i| {
                let q = 25_000.0 * i as f64 / 48.0;
                750_000_000.0 * q * (25_000.0 + 10_290.0) / (25_000.0 * (q + 10_290.0))
            })
            .collect();
        let e18 = |n: u128| wallet_core::sdk::U256::from(n) * wallet_core::sdk::U256::from(10u128.pow(18));
        app.eco.curves.insert(
            cheez.token.clone(),
            Ok(wallet_core::curve::CurveMarket {
                token_decimals: 18,
                token: cheez.token.clone(),
                curve: cheez.curve.clone().unwrap(),
                curve_tokens: e18(750_000_000),
                tokens_sold: e18(385_000_000),
                raised: e18(12_837),
                target: e18(25_000),
                fee_bps: 60,
                spot_price: 0.0000517,
                progress_bps: 5135,
                graduated: false,
                points: wallet_core::curve::price_points(25_000.0, &sold),
                held: e18(1_250_000),
                claimable: e18(3),
            }),
        );
        app.eco.launches =
            Some(Ok(vec![cheez, launch("QOGE", Phase::Graduated, Some(10_000), Some(0.00027)), launch("PUNK", Phase::Pooled, None, None)]));
    }
    // Both markets quoted for 5,000 QUAI: the market route pays more, the protocol is selected.
    {
        use wallet_core::qi_market::{Comparison, Direction, Leg, Route};
        let route = |name: &str, receives: &str, display: &str, legs: &[&str], wait: &str| Route {
            name: name.into(),
            receives: Some(receives.into()),
            receives_display: Some(display.into()),
            legs: legs.iter().map(|l| Leg { label: (*l).into(), detail: String::new() }).collect(),
            wait: wait.into(),
            costs: vec!["no LP fee or price impact".into()],
            unavailable: None,
            warnings: vec!["conversions in one prime block share a discount; one above your slippage is refunded".into()],
        };
        app.eco.convert.amount = "5000".into();
        app.eco.convert.routes = Some(Ok(Comparison {
            direction: Direction::QuaiToQi,
            amount: "5000000000000000000000".into(),
            amount_display: "5000 QUAI".into(),
            protocol: route("protocol conversion", "1432118", "1432.118 Qi", &["convert QUAI → Qi"], "locked by the protocol (weeks)"),
            market: route(
                "market route (Quainance)",
                "12893000",
                "12893.000 Qi",
                &["wrap QUAI → WQUAI", "swap WQUAI → WQI", "unwrap WQI → Qi"],
                "in minutes",
            ),
            market_advantage_bps: Some(80_026),
            observed_at: 1,
        }));
    }
    for tier in [super::super::terminal::Tier::Pixels, super::super::terminal::Tier::Cells, super::super::terminal::Tier::Text] {
        app.caps.tier = tier;
        for theme_id in ["terminal", "catppuccin-latte", "tokyo-night"] {
            app.theme = super::super::theme::resolve(app.paths.root(), theme_id, false, false).0;
            for (w, h) in [(60u16, 18u16), (80, 24), (100, 30), (160, 48)] {
                let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
                for screen in Screen::ALL {
                    app.switch(screen);
                    for filter in super::super::app::ActivityFilter::ALL {
                        app.activity_filter = filter;
                        for pane in 0..2 {
                            app.pane = pane;
                            app.modal = Modal::None;
                            term.draw(|f| draw(f, &mut app)).unwrap();
                        }
                        if screen != Screen::Activity {
                            break;
                        }
                    }
                    for m in modals(&app) {
                        app.modal = m;
                        term.draw(|f| draw(f, &mut app)).unwrap();
                    }
                    app.modal = Modal::TokenPicker { pay: true, query: "w".into(), selected: 1 };
                    term.draw(|f| draw(f, &mut app)).unwrap();
                    app.modal = Modal::None;
                    // Focused card fields.
                    app.eco.swap.field = 1;
                    app.eco.convert.field = 1;
                    app.eco.wrap.field = 1;
                    term.draw(|f| draw(f, &mut app)).unwrap();
                    app.unfocus_cards();
                }
                for d in &details {
                    app.detail = vec![d.clone()];
                    term.draw(|f| draw(f, &mut app)).unwrap();
                    let _ = super::super::images::kitty_items(&app);
                }
                app.detail.clear();
                app.help_moved = true;
                app.modal = Modal::Help;
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.help_moved = false;
                app.modal = Modal::None;
                app.locked = true;
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.locked = false;
                app.onboarding = Some(super::super::app::Onboarding::Choose { selected: 1 });
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.onboarding = Some(super::super::app::Onboarding::Theme(Picker::new(&app)));
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.onboarding = Some(super::super::app::Onboarding::Privacy { selected: 0 });
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.onboarding = Some(super::super::app::Onboarding::Welcome);
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.onboarding = Some(super::super::app::Onboarding::Motion { selected: 1, from: wallet_core::config::Motion::Vivid });
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.onboarding = None;
            }
        }
    }
    // The portfolio total and holdings are on Home; images went to kitty in the pixel tier.
    app.caps.tier = super::super::terminal::Tier::Pixels;
    app.switch(Screen::Home);
    let mut term = Terminal::new(TestBackend::new(160, 48)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(!super::super::images::kitty_items(&app).is_empty(), "token icon placed as a kitty bitmap");
    // Bundled QUAI / Qi logos replace inline badges (header, activity) once loaded.
    for asset in ["quai", "qi"] {
        let url = wallet_core::media::native_icon(asset).unwrap();
        let wallet_core::media::Source::Inline(bytes) = wallet_core::media::resolve(url).unwrap() else { panic!("inline") };
        let r = std::sync::Arc::new(wallet_core::media::make_rendition(&bytes, wallet_core::media::ICON).unwrap());
        app.eco
            .images
            .insert((url.to_string(), wallet_core::media::ICON), super::super::eco::ImageSlot::Ready(r, std::time::Instant::now()));
    }
    app.switch(Screen::Home);
    term.draw(|f| draw(f, &mut app)).unwrap();
    let placed = super::super::images::kitty_items(&app);
    assert!(
        placed.iter().any(|p| (p.x, p.y, p.cols, p.rows) == (1, 0, 2, 1)),
        "header logo placed: {:?}",
        placed.iter().map(|p| (p.x, p.y, p.cols, p.rows)).collect::<Vec<_>>()
    );
    assert_eq!(term.backend().buffer().cell((1, 0)).map(|c| c.symbol()), Some(" "), "badge letters cleared under the logo");
    // Filament: the focused border is a gradient on truecolor hex themes; Vivid turns it,
    // and a modal stops it.
    let saved_theme = app.theme.clone();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    app.caps.truecolor = true;
    // Over SSH or inside tmux the wallet calms motion itself; this checks the local case.
    app.caps.ssh = false;
    app.caps.tmux = false;
    app.config.motion = Motion::Vivid;
    // Someone is here: the light rests a minute after the last key, and the sweep above can
    // take that long on a slow machine.
    app.last_input = std::time::Instant::now();
    term.draw(|f| draw(f, &mut app)).unwrap();
    // The pass has already recolored the focused corner, so look for the lit border itself.
    let buf = term.backend().buffer();
    let edge_colors: std::collections::HashSet<_> = super::super::edge::panels(buf, &app.theme)
        .into_iter()
        .map(|p| {
            super::super::edge::perimeter(p.rect)
                .into_iter()
                .filter_map(|(x, y)| buf.cell((x, y)).filter(|c| c.symbol() == "─" || c.symbol() == "│").map(|c| c.fg))
                .collect::<std::collections::HashSet<_>>()
        })
        .max_by_key(|colors| colors.len())
        .unwrap_or_default();
    assert!(edge_colors.len() > 3, "gradient border: {edge_colors:?}");
    assert!(app.eco.anim_step.get().is_some(), "Vivid border schedules redraws");
    assert!(
        term.backend().buffer().cell((5, 0)).is_some_and(|c| c.modifier.contains(ratatui::style::Modifier::UNDERLINED)),
        "header hairline"
    );
    // QW_DUMP_PNG=dir rasterizes a few frames (cell colors, border strokes, text as blocks).
    if let Ok(dir) = std::env::var("QW_DUMP_PNG") {
        for (name, screen, clock) in [
            ("home_0", Screen::Home, 3_000u64),
            ("home_turn", Screen::Home, 7_000),
            ("tokens_glint", Screen::Home, 11_600),
            ("markets", Screen::Markets, 2_000),
        ] {
            app.switch(screen);
            app.edge_intro = None;
            app.last_input = std::time::Instant::now() - std::time::Duration::from_secs(10);
            app.eco.anim_ms = clock;
            term.draw(|f| draw(f, &mut app)).unwrap();
            raster(term.backend().buffer(), &format!("{dir}/{name}.png"));
        }
        app.switch(Screen::Home);
        app.edge_intro = Some(std::time::Instant::now() - std::time::Duration::from_millis(110));
        term.draw(|f| draw(f, &mut app)).unwrap();
        raster(term.backend().buffer(), &format!("{dir}/intro_mid.png"));
        app.edge_intro = None;
    }
    app.modal = Modal::Help;
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.eco.anim_step.get().is_none(), "nothing turns under a modal");
    app.modal = Modal::None;
    app.config.motion = Motion::Off;
    app.theme = saved_theme;
    // Under a modal the header keeps its logo (it is chrome, not dimmed), and nothing is placed
    // behind the glass: a bitmap can't dim with the page.
    app.modal = Modal::Help;
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert_eq!(term.backend().buffer().cell((1, 0)).map(|c| c.symbol()), Some(" "), "the logo, not the ◆ glyph");
    let placed = super::super::images::kitty_items(&app);
    assert!(placed.iter().any(|p| (p.x, p.y) == (1, 0)), "header logo still placed");
    assert!(placed.iter().all(|p| p.y == 0), "nothing behind the glass: {:?}", placed.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>());
    app.modal = Modal::None;
    app.caps.tier = super::super::terminal::Tier::Cells;
    term.draw(|f| draw(f, &mut app)).unwrap();
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("1,284") || text.contains("█▀█"), "total rendered");
    assert!(text.contains("WQI") && text.contains("not counted in the total"), "holdings strip and NFT strip");
    // Golden files: every screen as text at three sizes, checked in under `tui/golden`, so a
    // layout change shows up as a diff in review. `QW_BLESS=1` rewrites them; QW_DUMP_VIEWS=dir
    // also writes the rest (details, lock states, cards) for manual review.
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui/golden");
    let bless = std::env::var("QW_BLESS").is_ok();
    let mut drift = Vec::new();
    {
        app.theme = super::super::theme::resolve(app.paths.root(), "terminal", false, false).0;
        // What the terminal was detected as comes from the environment, which CI does not share.
        app.caps.terminal = "xterm-256color".into();
        app.caps.truecolor = true;
        app.caps.tmux = false;
        app.caps.ssh = false;
        app.caps.kitty_keyboard = false;
        for (w, h) in [(160u16, 48u16), (100, 30), (80, 24)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            for screen in Screen::ALL {
                // Data sources shows this process's live request counts, which other tests move.
                if screen == Screen::DataSources {
                    continue;
                }
                app.switch(screen);
                // Each screen as first opened: screens remember their pane and cursor now.
                app.pane = 0;
                app.selected = 0;
                app.eco.swap.field = 5;
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                let out: String = (0..h)
                    .map(|y| {
                        (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>().trim_end().to_string()
                            + "\n"
                    })
                    .collect();
                // Spinners turn with the clock and the data dir is a fresh temp path: neither is layout.
                // Clock times are local and the fixtures are dated from now, so they are masked too.
                let out = mask_clock(&mask_data_dir(&out.replace(|c| "⠇⠋⠏⠙⠦⠧⠴⠸⠹⠼".contains(c), "⠋"), app.paths.root()));
                let file = golden.join(format!("{screen:?}_{w}x{h}.txt"));
                if bless {
                    std::fs::create_dir_all(&golden).unwrap();
                    std::fs::write(&file, &out).unwrap();
                } else if std::fs::read_to_string(&file).ok().as_deref() != Some(out.as_str()) {
                    std::fs::write(file.with_extension("actual"), &out).unwrap();
                    drift.push(file.display().to_string());
                }
            }
            // The review is where money moves: its layout is held to the same standard.
            for (i, m) in modals(&app).into_iter().filter(|m| matches!(m, Modal::Review(_))).enumerate() {
                app.switch(Screen::Home);
                app.modal = m;
                // The read meter fills with time as well as scrolling; hold the clock still.
                if let Modal::Review(r) = &mut app.modal {
                    r.opened = std::time::Instant::now() - std::time::Duration::from_secs(600);
                }
                term.draw(|f| draw(f, &mut app)).unwrap();
                app.modal = Modal::None;
                let buf = term.backend().buffer();
                let out: String = (0..h)
                    .map(|y| {
                        (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>().trim_end().to_string()
                            + "\n"
                    })
                    .collect();
                let out = mask_clock(&out.replace(|c| "⠇⠋⠏⠙⠦⠧⠴⠸⠹⠼".contains(c), "⠋"));
                let file = golden.join(format!("review{i}_{w}x{h}.txt"));
                if bless {
                    std::fs::write(&file, &out).unwrap();
                } else if std::fs::read_to_string(&file).ok().as_deref() != Some(out.as_str()) {
                    std::fs::write(file.with_extension("actual"), &out).unwrap();
                    drift.push(file.display().to_string());
                }
            }
        }
        // Plain (NO_COLOR, `--plain`, the Linux console): no color at all, ASCII marks, nothing
        // moving. Every meaning must still be on the screen as text; these hold that.
        let (saved_theme, saved_plain) = (app.theme.clone(), app.plain);
        app.theme = super::super::theme::Theme::mono();
        app.plain = true;
        let (w, h) = (80u16, 24u16);
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let review = modals(&app).into_iter().find(|m| matches!(m, Modal::Review(_)));
        for (name, screen, modal) in [
            ("Home", Screen::Home, None),
            ("Activity", Screen::Activity, None),
            ("Accounts", Screen::Accounts, None),
            ("review", Screen::Home, review),
        ] {
            app.switch(screen);
            app.pane = 0;
            app.selected = 0;
            if let Some(mut m) = modal {
                if let Modal::Review(r) = &mut m {
                    r.opened = std::time::Instant::now() - std::time::Duration::from_secs(600);
                }
                app.modal = m;
            }
            term.draw(|f| draw(f, &mut app)).unwrap();
            app.modal = Modal::None;
            let buf = term.backend().buffer();
            let out: String = (0..h)
                .map(|y| {
                    (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>().trim_end().to_string() + "\n"
                })
                .collect();
            let out = mask_clock(&mask_data_dir(&out, app.paths.root()));
            let file = golden.join(format!("plain_{name}_{w}x{h}.txt"));
            if bless {
                std::fs::write(&file, &out).unwrap();
            } else if std::fs::read_to_string(&file).ok().as_deref() != Some(out.as_str()) {
                std::fs::write(file.with_extension("actual"), &out).unwrap();
                drift.push(file.display().to_string());
            }
        }
        app.theme = saved_theme;
        app.plain = saved_plain;
    }
    assert!(drift.is_empty(), "screens differ from their golden files (QW_BLESS=1 to accept):\n{}", drift.join("\n"));
    if let Ok(dir) = std::env::var("QW_DUMP_VIEWS") {
        for (w, h) in [(160u16, 48u16), (100, 30), (80, 24)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            let mut shots: Vec<(String, Screen, Option<super::super::app::Detail>, usize)> =
                Screen::ALL.iter().map(|s| (format!("{s:?}"), *s, None, 0)).collect();
            for (i, d) in details.iter().enumerate() {
                shots.push((format!("detail{i}"), Screen::Home, Some(d.clone()), 0));
            }
            shots.push(("swap_focused".into(), Screen::Swap, None, 1));
            for (name, screen, detail, focus) in shots {
                app.switch(screen);
                app.eco.swap.field = if focus == 1 { 1 } else { 5 };
                if let Some(d) = detail {
                    app.detail = vec![d];
                }
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                let mut out = String::new();
                for y in 0..h {
                    for x in 0..w {
                        out.push_str(buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" "));
                    }
                    out.push('\n');
                }
                std::fs::write(format!("{dir}/{name}_{w}x{h}.txt"), out).unwrap();
                app.detail.clear();
            }
            // The lock screen in each of the states it can answer in.
            for (name, unlocking, error) in
                [("lock_unlocking", true, None), ("lock_wrong", false, Some(app::friendly_error("incorrect password or corrupted vault")))]
            {
                app.locked = true;
                app.ambient = None;
                app.unlocking = unlocking;
                app.lock_error = error;
                app.lock_input = "hunter2".into();
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                let out: String = (0..h)
                    .map(|y| (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>() + "\n")
                    .collect();
                std::fs::write(format!("{dir}/{name}_{w}x{h}.txt"), out).unwrap();
            }
            app.locked = false;
            app.unlocking = false;
            app.lock_error = None;
            app.lock_input.clear();
            // The deposit card, which only exists while one is being composed.
            {
                let tok = |s: &str, d: u8| wallet_core::markets::PoolToken { address: format!("0x00{s}"), symbol: s.into(), decimals: d };
                app.switch(Screen::Pools);
                app.eco.pools_view.add = Some(super::super::eco::AddCard {
                    pair: "0x00pair".into(),
                    name: "SMOL/WQI".into(),
                    token0: tok("SMOL", 18),
                    token1: tok("WQI", 18),
                    side1: true,
                    amount: "3.5".into(),
                    slippage_bps: 50,
                    account: None,
                    field: 2,
                    quote: None,
                    quote_key: 0,
                    requested_key: 0,
                    edited: None,
                });
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                let out: String = (0..h)
                    .map(|y| (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>() + "\n")
                    .collect();
                std::fs::write(format!("{dir}/pools_add_{w}x{h}.txt"), out).unwrap();
                app.eco.pools_view.add = None;
            }
            for (i, m) in modals(&app).into_iter().filter(|m| matches!(m, Modal::Review(_))).enumerate() {
                app.switch(Screen::Home);
                app.modal = m;
                term.draw(|f| draw(f, &mut app)).unwrap();
                let buf = term.backend().buffer();
                let out: String = (0..h)
                    .map(|y| (0..w).map(|x| buf.cell((x, y)).map(|c| c.symbol()).unwrap_or(" ")).collect::<String>() + "\n")
                    .collect();
                std::fs::write(format!("{dir}/review{i}_{w}x{h}.txt"), out).unwrap();
                app.modal = Modal::None;
            }
        }
    }
}

/// The temp data dir → `<data>`, as drawn: shortened past 40 characters (a macOS temp path is),
/// and padded to the width it took so the border after it stays put whatever the path's length.
fn mask_data_dir(s: &str, root: &std::path::Path) -> String {
    // A wallet's directory is drawn as one path, shortened (`…`) when the temp dir is long, as it
    // is on macOS. Mask the whole path whatever its length, keeping each line's width so the border
    // after it stays put.
    let s: String = s
        .split_inclusive('\n')
        .map(|line| {
            let Some(at) = line.find("wallets/") else { return line.to_string() };
            let id = line[at + "wallets/".len()..].chars().take_while(char::is_ascii_hexdigit).count();
            if id == 0 {
                return line.to_string();
            }
            let start = line[..at].rfind(' ').map_or(0, |i| i + 1);
            let end = at + "wallets/".len() + id;
            let (old, new) = (line[start..end].chars().count(), "<data>/wallets/<id>");
            let tail = &line[end..];
            let spaces = tail.chars().take_while(|c| *c == ' ').count();
            // Pad or trim the spaces after the path by the difference.
            let pad = (spaces + old).saturating_sub(new.len());
            format!("{}{new}{}{}", &line[..start], " ".repeat(pad), &tail[spaces..])
        })
        .collect();
    let root = root.display().to_string();
    let mut out = s;
    for shown in [super::super::app::short_path(&root), root] {
        let mask = format!("{:<1$}", "<data>", shown.chars().count());
        out = out.replace(&shown, &mask);
    }
    // A wallet's directory is named by its id, which is new every run.
    let mut masked = String::new();
    let mut rest = out.as_str();
    while let Some(at) = rest.find("/wallets/") {
        let (head, tail) = rest.split_at(at + "/wallets/".len());
        masked.push_str(head);
        let id = tail.chars().take_while(char::is_ascii_hexdigit).count();
        if id > 0 {
            masked.push_str(&format!("{:<1$}", "<id>", id));
        }
        rest = &tail[id..];
    }
    masked.push_str(rest);
    masked
}

/// `14:05` → `hh:mm`, wherever a clock time appears.
fn mask_clock(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < c.len() {
        let digit = |k: usize| c.get(k).is_some_and(|d| d.is_ascii_digit());
        let edge = |k: Option<usize>| k.and_then(|k| c.get(k)).is_none_or(|d| !d.is_ascii_digit());
        if digit(i)
            && digit(i + 1)
            && c.get(i + 2) == Some(&':')
            && digit(i + 3)
            && digit(i + 4)
            && edge(i.checked_sub(1))
            && edge(Some(i + 5))
        {
            out.push_str("hh:mm");
            i += 5;
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// Every non-ASCII character in the TUI's source is drawn by common Nerd Fonts (checked
/// against JetBrainsMono Nerd Font). Characters outside this set fall back to other fonts (CJK,
/// DejaVu) with different widths and metrics, and render oversized or clipped.
#[test]
fn glyphs_stay_in_the_nerd_font_set() {
    const ALLOWED: &str = GLYPHS;
    let mut stack = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
    let mut offenders = Vec::new();
    while let Some(path) = stack.pop() {
        for entry in std::fs::read_dir(&path).unwrap().flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "rs") {
                for (n, line) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    for ch in code.chars().filter(|c| !c.is_ascii() && !ALLOWED.contains(*c)) {
                        offenders.push(format!("{}:{} {ch} U+{:04X}", p.display(), n + 1, ch as u32));
                    }
                }
            }
        }
    }
    assert!(offenders.is_empty(), "glyphs outside the Nerd Font set:\n{}", offenders.join("\n"));
}

/// The non-ASCII glyphs any monospace font here draws: the Unicode icon set lives inside it, and
/// Nerd Font icons are written only as escapes in `icons.rs`.
pub(crate) const GLYPHS: &str = "━╍±·»×èéê–—‖“”•…‹›←↑→↓↔↕↗↘↩−≈≋≤─│┃┈┊┌┐└┘├┤┬┴┼▀▁▂▃▄▅▆▇█▉▊▋▌▍▎▏░▒▔■□▪▲▸▼▾◂◆◇◈◉◊○◌◎●◔◕◦◧⚠✓✕⠇⠋⠏⠙⠦⠧⠴⠸⠹⠼";

/// Rough raster of a buffer for visual review: 8×16 cells, box glyphs as strokes, other glyphs
/// as blocks, underlines as a bottom line.
fn raster(buf: &Buffer, path: &str) {
    let (cw, ch) = (8u32, 16u32);
    let (w, h) = (u32::from(buf.area.width) * cw, u32::from(buf.area.height) * ch);
    let mut px = vec![0u8; (w * h * 4) as usize];
    let rgb = |c: Color, d: (u8, u8, u8)| if let Color::Rgb(r, g, b) = c { (r, g, b) } else { d };
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let bg = rgb(cell.bg, (8, 6, 6));
            let fg = rgb(cell.fg, (220, 220, 220));
            let sym = cell.symbol();
            let (up, down, left, right) = match sym {
                "─" => (false, false, true, true),
                "│" => (true, true, false, false),
                "┌" => (false, true, false, true),
                "┐" => (false, true, true, false),
                "└" => (true, false, false, true),
                "┘" => (true, false, true, false),
                _ => (false, false, false, false),
            };
            let boxy = up || down || left || right;
            for py in 0..ch {
                for pxl in 0..cw {
                    let (mx, my) = (cw / 2, ch / 2);
                    let stroke = boxy
                        && ((pxl == mx && ((up && py <= my) || (down && py >= my)))
                            || (py == my && ((left && pxl <= mx) || (right && pxl >= mx))));
                    let text = !boxy && sym != " " && !sym.is_empty() && (1..7).contains(&pxl) && (4..13).contains(&py);
                    let under = cell.modifier.contains(Modifier::UNDERLINED) && py == ch - 1;
                    let c = if stroke || text {
                        fg
                    } else if under {
                        rgb(cell.underline_color, fg)
                    } else {
                        bg
                    };
                    let i = (((u32::from(y) * ch + py) * w + u32::from(x) * cw + pxl) * 4) as usize;
                    px[i..i + 4].copy_from_slice(&[c.0, c.1, c.2, 255]);
                }
            }
        }
    }
    std::fs::write(path, super::super::images::encode_rgba(w, h, &px).unwrap()).unwrap();
}

#[test]
fn confirmation_tally() {
    let mut op = wallet_core::appdb::Operation {
        id: "a".into(),
        network: "local".into(),
        kind: "send_quai".into(),
        store: "quai".into(),
        account: "0x00".into(),
        status: OpStatus::Confirmed,
        tx_hash: None,
        asset: "QUAI".into(),
        amount: "1".into(),
        counterparty: String::new(),
        fee: String::new(),
        detail: serde_json::json!({"included_block": 100}),
        created: 0,
        updated: 0,
    };
    assert_eq!(confirmations(&op, 102), Some((3, 5)));
    assert_eq!(tally(3, 5), "━━━╍╍ 3/5");
    assert_eq!(confirmations(&op, 104), Some((5, 5)));
    assert_eq!(confirmations(&op, 110), None, "deep enough: plain status");
    assert_eq!(confirmations(&op, 90), None, "head behind the block");
    op.status = OpStatus::Submitted;
    assert_eq!(confirmations(&op, 102), None, "not mined yet");
}

#[test]
fn address_grouping() {
    assert_eq!(group4("0x00ab12cd"), "0x 00ab 12cd");
}

#[cfg(test)]
fn screen_text(app: &mut App, w: u16, h: u16) -> Vec<String> {
    use ratatui::{Terminal, backend::TestBackend};
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| draw(f, app)).unwrap();
    let buffer = term.backend().buffer();
    (0..h).map(|y| (0..w).filter_map(|x| buffer.cell((x, y)).map(|c| c.symbol().to_string())).collect::<String>()).collect()
}

/// A wide terminal shows the selected row's detail in a column beside the list; a narrower one
/// stacks it under the list when there are rows to spare. The time locks, which were their own
/// tab, sit under the accounts at every size.
#[test]
fn the_selected_row_is_inspected_beside_or_under_the_list() {
    let (_dir, mut app) = populated_app();
    app.switch(Screen::Accounts);
    let label = app.dash.accounts[0].label.clone();
    let title = format!("account · {label}");
    let find = |rows: &[String], needle: &str| rows.iter().position(|r| r.contains(needle));
    let wide = screen_text(&mut app, 160, 48);
    assert_eq!(app.breakpoint, Breakpoint::Wide);
    let list_row = find(&wide, "quai accounts").expect("the list");
    let inspector_row = find(&wide, &title).expect("the inspector");
    assert_eq!(list_row, inspector_row, "side by side at 160 columns");
    assert!(wide[inspector_row].find(&title) > wide[list_row].find("quai accounts"), "the inspector is on the right");
    assert!(find(&wide, "time locks").is_some(), "the locks sit under the accounts");
    let regular = screen_text(&mut app, 100, 30);
    assert_eq!(app.breakpoint, Breakpoint::Regular);
    assert!(find(&regular, &title) > find(&regular, "time locks"), "stacked under the list and the locks");
    // The panels are as tall as what they hold: one account is one row, not the whole screen.
    let locks = find(&regular, "time locks").unwrap();
    assert!(locks - find(&regular, "quai accounts").unwrap() <= 4, "the accounts panel holds its rows and no more");
    assert_eq!(Screen::Accounts.section().all_screens(), &[Screen::Home, Screen::Qi, Screen::Accounts], "no separate locks tab");
}

/// Where the terminal draws text larger than a cell, Home's total is sized text over placeholder
/// cells, not block digits; under a modal it is block digits again, so the dimming covers it.
#[test]
fn the_headline_total_is_sized_text_where_the_terminal_can() {
    use super::super::term::backend::BIG_TEXT_CELL;
    let (_dir, mut app) = populated_app();
    app.switch(Screen::Home);
    app.caps.text_sizing = true;
    let rows = screen_text(&mut app, 160, 48);
    let sized = app.big_text.borrow().clone();
    assert_eq!(sized.len(), 1, "one headline");
    let b = &sized[0];
    assert!(b.text.starts_with('$') && b.text.contains(',') && !b.text.contains('.'), "dollars, grouped, cents apart: {:?}", b.text);
    assert_eq!(b.scale, 2);
    let row = &rows[b.y as usize];
    let placeholders = row.matches(BIG_TEXT_CELL).count() as u16;
    assert_eq!(placeholders, b.width(), "its cells hold its place: {row:?}");
    assert!(rows[b.y as usize + 1].contains('.'), "the cents sit beside it on its lower row");
    // A modal: block digits, nothing sized.
    app.modal = Modal::Help;
    let rows = screen_text(&mut app, 160, 48);
    assert!(app.big_text.borrow().is_empty(), "nothing sized under a modal");
    assert!(!rows.iter().any(|r| r.contains(BIG_TEXT_CELL)));
    app.modal = Modal::None;
    // Without the capability, never.
    app.caps.text_sizing = false;
    screen_text(&mut app, 160, 48);
    assert!(app.big_text.borrow().is_empty());
}

/// The wallet's own addresses link to the explorer wherever they are shown: grouped in the
/// inspector, shortened in the table. The link covers exactly what shows the address.
#[test]
fn the_wallets_addresses_link_to_the_explorer() {
    let (_dir, mut app) = populated_app();
    app.caps.hyperlinks = true;
    // A network with an explorer (the fixture's local one has none, and links nothing).
    app.network_id = "mainnet".into();
    app.switch(Screen::Accounts);
    let rows = screen_text(&mut app, 160, 48);
    let buffer = {
        use ratatui::{Terminal, backend::TestBackend};
        let mut term = Terminal::new(TestBackend::new(160, 48)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        term.backend().buffer().clone()
    };
    let links = super::super::links::scan(&app, &buffer);
    let address = app.dash.accounts[0].address.to_lowercase();
    let net = app.net();
    let url = net.as_ref().and_then(|n| n.address_url(&address)).expect("mainnet has an explorer");
    let shown: Vec<String> = links
        .iter()
        .filter(|l| l.url.eq_ignore_ascii_case(&url))
        .map(|l| rows[l.y as usize].chars().skip(l.x as usize).take((l.end - l.x) as usize).collect())
        .collect();
    assert!(shown.iter().any(|s| s.contains('…')), "the shortened one in the table: {shown:?}");
    assert!(shown.iter().any(|s| s.starts_with("0x ") && s.len() > 20), "the grouped one in the inspector: {shown:?}");
    assert!(shown.iter().all(|s| s.starts_with("0x") && !s.ends_with(' ')), "exactly the address: {shown:?}");
    app.caps.hyperlinks = false;
    screen_text(&mut app, 160, 48);
}

/// System › Network shows the chain beside the node: hashrate per algorithm, transactions and gas
/// over time, each with its figures in words as well as a chart. Every row keeps the terminal's
/// width at a narrow size, where the chain panel steps aside rather than squeezing node health.
#[test]
fn the_network_screen_shows_hashrate_transactions_and_gas() {
    use wallet_core::chainstats::{ChainStats, Hashrates, Hour};
    let (_dir, mut app) = drawable_app();
    app.dash.health = Some(wallet_core::network::NodeHealth {
        network: "mainnet".into(),
        chain_id: "9".into(),
        genesis: "0xff".into(),
        identity_ok: true,
        height: 10_154_714,
        head_hash: "0x01".into(),
        head_age_secs: Some(3),
        gas_price: "38566651549076".into(),
        client_version: Some("go-quai/v0.55".into()),
        latency_ms: 4,
        order: Some(2),
    });
    let rates = |k: f64| Hashrates { sha: 2.1e17 * k, scrypt: 1.07e13 * k, kawpow: 2.06e11 * k };
    let hours: Vec<Hour> = (0..48u64)
        .map(|i| Hour {
            at: 1_789_500_000 + i * 3600,
            blocks: 700,
            transactions: 6_600 + (i % 7) * 150,
            quai_transactions: 520,
            qi_transactions: 100,
            total_transactions: Some(100_359_518 + i * 7_000),
            gas_used: 690_000_000.0,
            fees_wei: 19_000e18 + (i as f64) * 1e20,
        })
        .collect();
    app.eco.chain_stats = Some(Ok(ChainStats {
        observed_at: wallet_core::registry::now() - 90,
        avg_block_secs: Some(5.25),
        total_transactions: Some(100_701_162),
        quai_addresses: Some(90_529),
        qi_addresses: Some(260_207),
        hashrate: rates(1.0),
        hashrate_history: (0..24u64).map(|i| (1_789_500_000 + i * 3600, rates(0.9 + (i % 5) as f64 * 0.05))).collect(),
        hours,
        block_reward_quai: Some(94.556),
    }));
    app.switch(Screen::Network);
    let wide = screen_text(&mut app, 160, 45);
    let all = wide.join("\n");
    for expected in [
        "100,701,162",
        "90,529 QUAI · 260,207 Qi",
        "94.56 QUAI",
        "SHA",
        "Scrypt",
        "KawPoW",
        "PH/s",
        "TH/s",
        "GH/s",
        "total 100.7M",
        "gas · node 38,567 gwei now",
        "48h ago",
        "as of 1m ago",
    ] {
        assert!(all.contains(expected), "missing {expected:?}:\n{all}");
    }
    if std::env::var("QW_SHOW").is_ok() {
        println!("{all}");
    }
    let narrow = screen_text(&mut app, 96, 40);
    assert!(narrow.iter().all(|l| l.chars().count() == 96), "rows keep the terminal's width");
    assert!(narrow.join("\n").contains("node health"), "node health keeps its room when narrow");
    // Before the statistics arrive the charts say so rather than drawing nothing.
    app.eco.chain_stats = None;
    assert!(screen_text(&mut app, 160, 45).join("\n").contains("loading"));
}

/// Launch-zone rows carry the token's logo slot before the symbol, and the columns stay aligned.
#[test]
fn launch_rows_carry_an_icon() {
    use wallet_core::launches::{Launch, Phase};
    let (_dir, mut app) = drawable_app();
    let launch = |token: &str, symbol: &str| Launch {
        venue_kind: Some(wallet_core::capabilities::Family::QuainanceCurve),
        price_basis: Default::default(),
        token: token.into(),
        symbol: symbol.into(),
        name: format!("{symbol} token"),
        decimals: 18,
        phase: Phase::Graduated,
        venue: "Quainance bonding curve".into(),
        curve: None,
        pair: None,
        progress_bps: Some(10_000),
        price_quai: Some(0.00005),
        raised_quai: 25_000.0,
        target_quai: Some(25_000.0),
        buys: 10,
        sells: 2,
        created_at: wallet_core::registry::now() - 3600,
        metadata_uri: Some("ipfs://bafkreigynbdigag634tzagcofoclwl7yqaehe4gq4s4gnx6yxal764fzfa".into()),
    };
    app.eco.launches = Some(Ok(vec![
        launch("0x0016c3221b6a1707427d660945cd284a9be58cec", "CHEEZ"),
        launch("0x0048848ca70ea1560577b4725a84b23b6bc589e2", "QOGE"),
    ]));
    app.eco.launch_logos.insert("0x0016c3221b6a1707427d660945cd284a9be58cec".into(), "https://www.quainance.com/api/media/bafy".into());
    assert_eq!(
        app.asset_icon_url("0x0016C3221B6A1707427D660945CD284A9BE58CEC").as_deref(),
        Some("https://www.quainance.com/api/media/bafy")
    );
    app.switch(Screen::Launches);
    let screen = screen_text(&mut app, 160, 45);
    let row = |sym: &str| screen.iter().find(|l| l.contains(sym)).cloned().unwrap_or_default();
    let (cheez, qoge) = (row("CHEEZ"), row("QOGE"));
    assert!(!cheez.is_empty() && !qoge.is_empty(), "{}", screen.join("\n"));
    // The symbol starts in the same column whether or not the logo has loaded.
    assert_eq!(cheez.find("CHEEZ").map(|i| cheez[..i].chars().count()), qoge.find("QOGE").map(|i| qoge[..i].chars().count()));
    if std::env::var("QW_SHOW").is_ok() {
        println!("{}", screen.join("\n"));
    }
}

/// A wallet that bought MOON twice, sold some, swapped a little into STAR (unpriced), and sold
/// CHEEZ it never bought here. Trades sit at noon UTC so their dates read the same in any zone
/// the goldens are drawn in.
fn sample_pnl() -> wallet_core::pnl::Pnl {
    use wallet_core::pnl::{Fill, Leg, compute};
    let leg = |token: &str, symbol: &str, units: f64| Leg { token: token.into(), symbol: symbol.into(), decimals: 18, units };
    let fill = |id: &str, at: u64, quai: f64, legs: Vec<Leg>| Fill {
        op_id: id.into(),
        at,
        tx: Some(format!("0x{id}")),
        kind: "swap".into(),
        account: "0xme".into(),
        quai,
        legs,
        estimated: false,
        fee: 0.05,
    };
    let fills = vec![
        fill("b1", 1_789_473_600, -100.0, vec![leg("0xmoon", "MOON", 1000.0)]),
        fill("b2", 1_789_560_000, -300.0, vec![leg("0xmoon", "MOON", 1000.0)]),
        fill("s1", 1_789_646_400, 150.0, vec![leg("0xmoon", "MOON", -500.0)]),
        fill("t1", 1_789_646_460, 0.0, vec![leg("0xmoon", "MOON", -100.0), leg("0xstar", "STAR", 25.0)]),
        fill("s2", 1_789_646_520, 12.5, vec![leg("0xcheez", "CHEEZ", -4_000_000.0)]),
    ];
    let marks = std::collections::HashMap::from([("0xmoon".to_string(), 0.4)]);
    compute(&fills, 0.2, &marks, &std::collections::HashMap::new())
}

/// The PnL screen leads with the net, lists each token with its cost and gains, and says under
/// the table what the focused token's figures leave out.
#[test]
fn pnl_shows_totals_positions_and_what_a_token_leaves_out() {
    let (_dir, mut app) = drawable_app();
    app.eco.pnl = Some(Ok(sample_pnl()));
    app.eco.pnl_at = Some(std::time::Instant::now());
    app.switch(Screen::Pnl);
    let screen = screen_text(&mut app, 160, 45);
    let text = screen.join("\n");
    // 50 realized on MOON, 280 unrealized on the 1,400 left, less 0.45 of gas.
    assert!(text.contains("net +329.55 QUAI"), "{text}");
    let row = |sym: &str| screen.iter().find(|l| l.contains(&format!(" {sym} ")) && !l.contains(" QUAI")).cloned().unwrap_or_default();
    let moon = row("MOON");
    assert!(moon.contains("1,400.0000") && moon.contains("+280.00") && moon.contains("+50.00"), "{moon}");
    assert!(row("STAR").contains('—'), "STAR has no price: {}", row("STAR"));
    assert!(row("CHEEZ").contains("closed"), "{}", row("CHEEZ"));
    assert!(text.contains("latest trades") && text.contains("−4.0M CHEEZ"), "{text}");
    assert!(text.contains("−100.0000 MOON  +25.0000 STAR"), "a token-for-token trade reads in full: {text}");
    // Focus CHEEZ: the sale with no recorded buy is said, not counted.
    app.selected = app.pnl_positions().iter().position(|p| p.symbol == "CHEEZ").unwrap();
    let text = screen_text(&mut app, 160, 45).join("\n");
    assert!(text.contains("CHEEZ: 4.0M sold with no buy recorded here"), "{text}");
    // Narrow: the price and trade columns give way, the figures stay.
    let narrow = screen_text(&mut app, 80, 24).join("\n");
    assert!(narrow.contains("MOON") && narrow.contains("+280.00"), "{narrow}");
    if std::env::var("QW_SHOW").is_ok() {
        println!("{text}\n\n{narrow}");
    }
}

/// Settings shows where each kind of IPFS content comes from, and what kind of source it is, in
/// words. ABIs and images are separate rows because they are separate settings.
#[test]
fn settings_name_the_ipfs_gateways() {
    let _gateway = crate::tui::IPFS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_dir, mut app) = drawable_app();
    app.switch(Screen::Settings);
    // The list scrolls to the cursor: stand on the gateways.
    app.selected = app.settings_rows().iter().position(|(id, _)| *id == "abi_ipfs").unwrap();
    let screen = screen_text(&mut app, 140, 40).join("\n");
    let rows: Vec<&str> = screen.lines().filter(|l| l.contains("IPFS gateway")).collect();
    assert_eq!(rows.len(), 2, "one row per gateway: {rows:?}");
    let images = rows.iter().find(|l| l.contains("pictures")).unwrap_or(&"");
    let abis = rows.iter().find(|l| l.contains("contracts")).unwrap_or(&"");
    assert!(images.contains("https://ipfs.qu.ai") && images.contains("default"), "{images}");
    assert!(abis.contains("https://ipfs.qu.ai") && abis.contains("default"), "{abis}");
}

/// An operation's detail tells its life in order: each stage, when, and how long after the last;
/// one still open ends on what it is waiting for.
#[test]
fn an_operation_shows_its_timeline() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::appdb::{OpStatus, Operation};
    let (_dir, mut app) = drawable_app();
    let t0 = wallet_core::registry::now() - 100;
    app.dash.ops = vec![Operation {
        id: "op1".into(),
        network: "local".into(),
        kind: "send_quai".into(),
        store: "quai".into(),
        account: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        status: OpStatus::Submitted,
        tx_hash: Some("0x00aa".into()),
        asset: "QUAI".into(),
        amount: "1000000000000000000".into(),
        counterparty: "0x0011".into(),
        fee: String::new(),
        detail: serde_json::json!({"timeline": [
            {"s": "prepared", "at": t0},
            {"s": "signed", "at": t0 + 19},
            {"s": "submitted", "at": t0 + 20},
            {"s": "replaced", "at": t0 + 68, "tx": "0x00bbccddeeff00112233445566778899aabbccdd"},
        ]}),
        created: t0,
        updated: t0 + 68,
    }];
    app.detail = vec![super::super::app::Detail::Activity("op:op1".into())];
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    let text = &text[text.find("timeline").expect("a timeline section")..];
    let at = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} missing"));
    assert!(at("prepared") < at("signed") && at("signed") < at("submitted") && at("submitted") < at("replaced"), "in order");
    assert!(text.contains("+19s") && text.contains("+48s"), "gaps between stages");
    assert!(text.contains("waiting for a block"), "an open operation says what it waits for");
    assert!(!text.contains("\"s\":"), "the raw timeline is not dumped as a detail key");
}

/// The trader layout puts Markets and the swap card side by side on a wide terminal, keeping the
/// pair the list was on; focus drops the sidebar; standard never splits.
#[test]
fn layouts_split_and_hide_as_asked() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = drawable_app();
    let text = |app: &mut App, w: u16| {
        let mut term = Terminal::new(TestBackend::new(w, 40)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    app.network_id = "mainnet".into();
    app.eco.markets_view.pools = Some(Ok((Vec::new(), wallet_core::markets::DexOverview::default())));
    app.switch(Screen::Markets);
    app.config.layout = "auto".into();
    let wide = text(&mut app, 220);
    assert!(app.trader && wide.contains("swap on Quainance") && wide.contains("markets · Quainance"), "auto splits at 220 columns");
    let narrow = text(&mut app, 160);
    assert!(!app.trader && !narrow.contains("swap on Quainance"), "but not at 160");
    app.config.layout = "trader".into();
    assert!(text(&mut app, 160).contains("swap on Quainance"), "trader splits from 140");
    app.switch(Screen::Swap);
    assert!(text(&mut app, 160).contains("markets · Quainance"), "and the Swap screen keeps Markets beside it");
    app.config.layout = "standard".into();
    assert!(!text(&mut app, 220).contains("markets · Quainance"), "standard never splits");
    app.config.layout = "focus".into();
    assert!(!text(&mut app, 220).contains("[ ] tabs"), "focus has no sidebar");
}

/// People › Channels lists channel offers above the registered channels, each with what is
/// waiting and how to answer, and the details panel says an offer is not a channel yet. At the
/// narrowest supported size every row still fits.
#[test]
fn channel_offers_render_above_channels_with_their_answer_keys() {
    let (_dir, mut app) = drawable_app();
    app.dash.unlocked = true;
    app.dash.offers = vec![wallet_core::ops::ChannelOffer {
        code: "PM8TJofferoffer".into(),
        found: wallet_core::sdk::U256::from(2_500u64),
        first_seen: 1,
        last_probe: 1,
        notified: true,
    }];
    app.dash.peers = vec![wallet_core::ops::PeerView {
        code: "PM8TJpeerpeer".into(),
        contact: Some("alice".into()),
        receive_addresses: 3,
        send_addresses: 1,
    }];
    app.switch(Screen::Channels);
    app.selected = 0;
    for (w, h) in [(160, 48), (100, 30), (80, 24)] {
        let text = screen_text(&mut app, w, h).join("\n");
        assert!(text.contains("1 offered"), "{w}x{h}: panel title counts offers\n{text}");
        let offer_row =
            text.lines().position(|l| l.contains("offered · s accepts")).unwrap_or_else(|| panic!("{w}x{h}: offer row\n{text}"));
        let peer_row = text.lines().position(|l| l.contains("alice")).unwrap_or_else(|| panic!("{w}x{h}: channel row\n{text}"));
        assert!(offer_row < peer_row, "{w}x{h}: offers come first");
        assert!(text.contains("2.5 Qi"), "{w}x{h}: what is waiting\n{text}");
    }
    let wide = screen_text(&mut app, 160, 48).join("\n");
    assert!(wide.contains("channel offer") && wide.contains("not a channel until you"), "details explain the offer\n{wide}");
    // The cursor on the channel below shows that channel, not the offer.
    app.selected = 1;
    let wide = screen_text(&mut app, 160, 48).join("\n");
    assert!(wide.contains("rescan") && !wide.contains("not a channel until you"), "{wide}");
}

/// The lock screen plays one effect when the wallet locks and then rests: an idle wallet spends
/// most of its life here, and effects chained forever held a core at a fifth. The end is a
/// cross-fade (the finished frame thins away over the still wordmark), never a cut, and nothing
/// starts again until the next lock.
#[test]
fn the_lock_effect_loops_or_plays_once_then_rests() {
    use ratatui::{Terminal, backend::TestBackend};
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let meta = registry.create_watch("t", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    let mut caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Pixels);
    caps.truecolor = true;
    let theme = super::super::theme::resolve(std::path::Path::new("/nonexistent"), "quai-dark", false, false).0;
    let mut app = App::new(paths, "local".into(), wallet_core::config::AppConfig::default(), theme, caps, Some(meta));
    app.locked = true;
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    app.tick((120, 40));
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.ambient.is_some(), "locking plays an effect");

    let (w, h) = app.ambient.as_ref().unwrap().size();
    // A ceremony with a single frame left, so the next draw is the one it ends on.
    let ending = || {
        let mut c = super::super::fx::Ceremony::new("rings", &super::super::fx::wordmark_block(), w, h, 1).unwrap();
        assert!(c.step(), "its one frame is the cap");
        assert!(!c.step(), "and it is spent");
        c
    };
    let inked = |term: &Terminal<TestBackend>| {
        let buf = term.backend().buffer();
        (1..=h).any(|y| (0..w).any(|x| buf.cell((x, y)).is_some_and(|c| c.symbol().trim() != "")))
    };

    // Looping (the default): the next effect starts in the very draw the last one ends in, with
    // the old frame dissolving over it. No held frame, no wait for a tick, and focus does not
    // matter: in the background it plays on.
    assert!(app.config.lock_loop, "looping is the default");
    for focused in [true, false] {
        app.focused = focused;
        app.ambient = Some(ending());
        app.lock_rested = false;
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.ambient.as_ref().is_some_and(|c| c.frame().is_none()), "focused {focused}: a fresh effect began at once");
        assert!(!app.lock_rested && app.lock_fade.is_some(), "focused {focused}: the old frame dissolves over it");
        assert!(inked(&term), "focused {focused}: never an empty canvas in between");
        assert!(super::wants_animation(&app), "focused {focused}: frames keep coming");
    }
    // Typing stills it; once the field is empty again the loop picks up, focused or not.
    app.lock_input.push('x');
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.ambient.is_none() && app.lock_rested, "nothing moves near a password");
    app.tick((120, 40));
    assert!(app.ambient.is_none(), "nothing starts near a password");
    app.lock_input.clear();
    // Whatever was fading has aged out with no frame drawn to clear it, as in the running terminal.
    app.lock_fade = Some(("·".into(), std::time::Instant::now() - std::time::Duration::from_millis(600)));
    app.focused = false;
    app.tick((120, 40));
    assert!(app.ambient.is_some() && !app.lock_rested, "the loop resumes, in the background too");

    // Played once: a finished effect is not replaced, and rests on the wordmark.
    app.config.lock_loop = false;
    app.focused = true;
    app.ambient = Some(ending());
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.ambient.is_none() && app.lock_rested, "a finished effect is not replaced");
    assert!(app.lock_fade.is_some(), "its last frame dissolves over the resting wordmark");
    assert!(inked(&term), "the resting screen is never an empty canvas");
    app.tick((120, 40));
    assert!(app.ambient.is_none(), "the tick does not restart it");
    app.locked = false;
    app.enter_lock(Some((120, 40)));
    assert!(app.ambient.is_some() && !app.lock_rested, "the next lock plays one again");
}

/// With a modal open, what is behind it goes quiet: no text behind keeps a full-strength color,
/// no border behind stays lit, and the page color itself is left alone (a see-through
/// background stays see-through). The modal is drawn at full strength.
#[test]
fn a_modal_dims_what_is_behind_it() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    let t = app.theme.clone();
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
    app.modal = Modal::Confirm {
        title: "Unfollow".into(),
        body: "Stop following #general?".into(),
        action: super::super::app::ConfirmAction::Unfollow("general".into()),
    };
    term.draw(|f| draw(f, &mut app)).unwrap();
    let buf = term.backend().buffer().clone();
    // The modal's box: everything within the raised cells' bounds, and its shadow.
    let raised: Vec<(u16, u16)> = (0..36).flat_map(|y| (0..120).map(move |x| (x, y))).filter(|p| buf[*p].bg == t.raised).collect();
    let (x0, x1) = (raised.iter().map(|p| p.0).min().unwrap(), raised.iter().map(|p| p.0).max().unwrap() + 1);
    let (y0, y1) = (raised.iter().map(|p| p.1).min().unwrap(), raised.iter().map(|p| p.1).max().unwrap() + 1);
    let inside = |x: u16, y: u16| (x0..=x1).contains(&x) && (y0..=y1).contains(&y);
    let mut modal_text = 0;
    for y in 0..36 {
        for x in 0..120 {
            let c = &buf[(x, y)];
            if inside(x, y) {
                if c.fg == t.text || c.fg == t.strong {
                    modal_text += 1;
                }
                continue;
            }
            // The footer row names the modal's keys and the header keeps the wallet in view:
            // both are chrome, and stay lit.
            if y == 35 || y == 0 {
                continue;
            }
            for lit in [t.text, t.strong, t.focus, t.danger, t.ok] {
                assert_ne!(c.fg, lit, "({x},{y}) {:?} behind the modal is at full strength", c.symbol());
            }
            assert!(c.bg != t.selection, "({x},{y}) the selection behind the modal is not dimmed");
            assert!(!c.modifier.contains(Modifier::BOLD), "({x},{y}) bold behind the modal");
        }
    }
    assert!(modal_text > 10, "the modal itself is drawn at full strength");
    // The page stays the page.
    assert_eq!(buf[(119, 20)].bg, t.surface);
}

/// The accent budget: the accent marks what has focus (the lit border and its title, the
/// selection marker, the active tab, the cursor, the primary button) and the keys to press. Values,
/// names, prices and headings are never in it. Counted per screen in the body (the chrome rows are
/// the tab strip, the header and the footer's key hints); a screen that paints data in the accent
/// blows the budget.
#[test]
fn the_accent_is_kept_for_focus_and_keys() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    let t = app.theme.clone();
    let (w, h) = (160u16, 48u16);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    for screen in Screen::ALL {
        app.switch(screen);
        app.modal = Modal::None;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut lit = Vec::new();
        // Rows 0–2 are the header and the two-row tab strip; row 3 is the focused box's own
        // title, which the budget allows.
        for y in 4..h - 1 {
            for x in 0..w {
                let c = &buf[(x, y)];
                if c.fg == t.focus && !"─│┌┐└┘├┤┬┴┼ ".contains(c.symbol()) {
                    lit.push(c.symbol().to_string());
                }
            }
        }
        assert!(lit.len() <= 40, "{screen:?} spends the accent on {} cells: {}", lit.len(), lit.concat());
    }
}

/// The glyph setting reaches the screen: Nerd Font icons in the rail and header where asked for,
/// nothing outside ASCII among the status marks in the ASCII set, and the Unicode set (what the
/// goldens are drawn with) has no Nerd Font icon at all.
#[test]
fn the_icon_setting_picks_the_glyphs() {
    use wallet_core::config::IconMode;
    let (_dir, mut app) = populated_app();
    let draw_text = |app: &mut App| -> String {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 36)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer();
        (0..36).map(|y| (0..120).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>() + "\n").collect()
    };
    let nerd = |s: &str| s.chars().any(|c| ('\u{f0000}'..='\u{fffff}').contains(&c));
    app.config.icons = IconMode::Unicode;
    assert!(!nerd(&draw_text(&mut app)), "the Unicode set draws no Nerd Font icon");
    app.config.icons = IconMode::Nerd;
    let text = draw_text(&mut app);
    assert!(text.contains(super::super::icons::Icon::Home.glyph(super::super::icons::Set::Nerd)), "the rail shows section icons:\n{text}");
    assert!(text.contains(&format!("{} watch-only", super::super::icons::Icon::Watching.glyph(super::super::icons::Set::Nerd))), "{text}");
    app.config.icons = IconMode::Ascii;
    let text = draw_text(&mut app);
    assert!(!nerd(&text));
    for mark in ["✓", "✕", "◌", "◔", "◕", "●", "○"] {
        assert!(!text.contains(mark), "{mark} in the ASCII set:\n{text}");
    }
}

/// The header drops whole segments as it narrows, never words: at every width it still names the
/// wallet, the node and that the wallet is watch-only, and nothing it shows is cut short.
#[test]
fn the_header_drops_segments_not_words() {
    let (_dir, mut app) = populated_app();
    for w in 60u16..=200 {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer();
        let top: String = (0..w).map(|x| buf[(x, 0)].symbol().to_string()).collect();
        for must in ["PREVIEW", "Local dev", "watch-only"] {
            assert!(top.contains(must), "{w} columns lost {must}: {top}");
        }
        for piece in top.split(" │ ").map(str::trim) {
            assert!(!piece.ends_with('…'), "{w} columns cut a segment: {top}");
        }
    }
}

/// Glyphs that mean something are written only in the icon registry: a screen asks for
/// `Icon::Ok`, never types `✓`, so every mark follows the icon setting and keeps one meaning.
/// (Comments, tests, the glossary's prose and the CLI theme preview may name them.)
#[test]
fn meaningful_glyphs_come_from_the_registry() {
    const MARKS: &str = "✓✕◌◔◕●○⚠◇◈◦◊";
    let mut offenders = Vec::new();
    let mut files = Vec::new();
    let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/tui")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            if entry.path().is_dir() {
                dirs.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
    }
    assert!(files.iter().any(|p| p.ends_with("ui/modals.rs")), "the scan reaches the screen modules");
    for p in files {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") || ["icons.rs", "glossary.rs", "theme.rs"].contains(&name.as_str()) {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        // Everything after `#[cfg(test)]` is tests.
        let code = text.split("#[cfg(test)]\nmod tests").next().unwrap_or(&text);
        for (n, line) in code.lines().enumerate() {
            let line = line.split("//").next().unwrap_or("");
            if let Some(c) = line.chars().find(|c| MARKS.contains(*c)) {
                offenders.push(format!("{name}:{} {c}  {}", n + 1, line.trim()));
            }
        }
    }
    assert!(offenders.is_empty(), "write these through icons::Icon:\n{}", offenders.join("\n"));
}

/// One lit panel per screen: the focus border says where the keys go, so two of them is a lie.
/// (Counted by top-left corners drawn in the focus color.)
#[test]
fn one_panel_is_lit() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    let focus = app.theme.focus;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    let mut lies = Vec::new();
    let mut seen = 0;
    for screen in Screen::ALL {
        app.switch(screen);
        for pane in 0..screen.panes().max(1) {
            app.pane = pane;
            app.modal = Modal::None;
            term.draw(|f| draw(f, &mut app)).unwrap();
            let buf = term.backend().buffer();
            let lit = (0..48u16)
                .flat_map(|y| (0..160u16).map(move |x| (x, y)))
                .filter(|p| buf[*p].symbol() == "┌" && buf[*p].fg == focus)
                .count();
            seen += lit;
            if lit > 1 {
                lies.push(format!("{screen:?} pane {pane}: {lit} lit panels"));
            }
        }
    }
    assert!(lies.is_empty(), "{}", lies.join("\n"));
    assert!(seen > 20, "the count finds lit panels at all ({seen})");
}

/// A decoration frame (only the edge light moved) is exactly the frame a full redraw would have
/// drawn at the same moment, on every screen: it relights the last content instead of drawing it.
#[test]
fn an_edges_only_frame_equals_a_full_one() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    app.caps.truecolor = true;
    app.config.motion = Motion::Vivid;
    app.focused = true;
    app.last_input = std::time::Instant::now() - std::time::Duration::from_secs(10);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    let mut relit = 0;
    for screen in Screen::ALL {
        app.switch(screen);
        // The border intro is a full-frame animation of its own; this compares the frames after it.
        app.edge_intro = None;
        app.modal = Modal::None;
        app.eco.anim_ms = 1_000;
        term.draw(|f| draw(f, &mut app)).unwrap();
        // A picture fading in or a border drawing itself in is a full frame by rule
        // (`wants_animation`); compare once those rest.
        while app.eco.fading() || app.animating() {
            std::thread::sleep(std::time::Duration::from_millis(50));
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
        if app.eco.anim_step.get().is_none() {
            continue;
        }
        app.eco.anim_ms = 5_000;
        term.draw(|f| draw_edges(f, &mut app)).unwrap();
        let edges = term.backend().buffer().clone();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let full = term.backend().buffer().clone();
        // Spinners turn with the wall clock between the two draws; a turned spinner is a full
        // frame by rule (`spinning`), so they are not what this compares.
        let spin = |c: &ratatui::buffer::Cell| "⠇⠋⠏⠙⠦⠧⠴⠸⠹⠼".contains(c.symbol());
        let diffs: Vec<String> = (0..48u16)
            .flat_map(|y| (0..160u16).map(move |x| (x, y)))
            .filter(|p| edges[*p] != full[*p] && !(spin(&edges[*p]) && spin(&full[*p])))
            .take(8)
            .map(|p| format!("{p:?} edges {:?} {:?} / full {:?} {:?}", edges[p].symbol(), edges[p].fg, full[p].symbol(), full[p].fg))
            .collect();
        assert!(diffs.is_empty(), "{screen:?}: the relit frame differs from a full one:\n{}", diffs.join("\n"));
        relit += 1;
    }
    assert!(relit > 10, "screens with moving edges were checked ({relit})");
}

/// Left alone, the edge light rests: after a minute without input nothing asks for frames, and
/// the next input wakes it.
#[test]
fn the_edge_light_rests_when_nobody_is_there() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    app.caps.truecolor = true;
    app.config.motion = Motion::Vivid;
    app.focused = true;
    app.switch(Screen::Home);
    app.edge_intro = None;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    app.last_input = std::time::Instant::now() - std::time::Duration::from_secs(5);
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.eco.anim_step.get().is_some(), "in use, the light turns");
    app.last_input = std::time::Instant::now() - super::super::edge::REST_AFTER - std::time::Duration::from_secs(1);
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.eco.anim_step.get().is_none(), "left alone, nothing asks for a frame");
    app.last_input = std::time::Instant::now();
    term.draw(|f| draw(f, &mut app)).unwrap();
    assert!(app.eco.anim_step.get().is_some(), "a key wakes it");
}

/// The frame budget, on the golden fixture: every screen draws a full frame and a decoration
/// frame (edges only) within budget at p90. A release-build check, since debug timings mean
/// nothing: `cargo test --release -p quai-terminal-cli frame_budget -- --ignored --nocapture`.
/// CI runs it. Measured here at about 1.1 ms full and 0.45 ms edges (p50), so the budget leaves
/// room for a slower CI machine and still fails a screen that suddenly costs several times more,
/// which is what the Explore view model did before it was cached.
#[test]
#[ignore = "timing; release builds only (CI runs it with --release --ignored)"]
fn frame_budget() {
    let (_dir, mut app) = populated_app();
    app.theme = super::super::theme::resolve(app.paths.root(), "quai-red", false, false).0;
    app.caps.truecolor = true;
    app.config.motion = Motion::Vivid;
    app.focused = true;
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    let at = |mut v: Vec<u128>, q: usize| {
        v.sort_unstable();
        v[v.len() * q / 100]
    };
    let mut report = Vec::new();
    let mut over = Vec::new();
    for screen in Screen::ALL {
        app.switch(screen);
        app.edge_intro = None;
        app.modal = Modal::None;
        // Warm the caches a first frame fills.
        for _ in 0..3 {
            term.draw(|f| draw(f, &mut app)).unwrap();
        }
        // Three rounds, keeping each measure's best: a slow neighbour on a shared machine spoils
        // a round, a real regression spoils every one.
        let (mut full90, mut edges90, mut full50, mut edges50) = (u128::MAX, u128::MAX, u128::MAX, u128::MAX);
        for round in 0..3u64 {
            let (mut full, mut edges) = (Vec::new(), Vec::new());
            for i in 0..40u64 {
                app.eco.anim_ms = 1_000 + (round * 40 + i) * 150;
                let t = std::time::Instant::now();
                term.draw(|f| draw(f, &mut app)).unwrap();
                full.push(t.elapsed().as_micros());
                let t = std::time::Instant::now();
                term.draw(|f| draw_edges(f, &mut app)).unwrap();
                edges.push(t.elapsed().as_micros());
            }
            full50 = full50.min(at(full.clone(), 50));
            edges50 = edges50.min(at(edges.clone(), 50));
            full90 = full90.min(at(full, 90));
            edges90 = edges90.min(at(edges, 90));
        }
        report.push(format!("{screen:?}: full p50 {full50} p90 {full90} us, edges p50 {edges50} p90 {edges90} us"));
        // Render plus ratatui's diff into the test backend.
        if full90 > 4_000 || edges90 > 1_500 {
            over.push(format!("{screen:?} full {full90} us edges {edges90} us"));
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(over.is_empty(), "over the frame budget:\n{}", over.join("\n"));
}

/// What the wallet owns is on Home: liquidity positions are holdings rows, their value is in the
/// total (and said to be), and Enter on one opens it on Pools, under the cursor.
#[test]
fn liquidity_positions_are_holdings_on_home() {
    let (_dir, mut app) = populated_app();
    app.switch(Screen::Home);
    let tokens = app.eco.portfolio.as_ref().unwrap().rows.len();
    let positions = app.home_positions().len();
    assert!(positions > 0, "the fixture holds positions");
    assert_eq!(app.list_len(), tokens + positions, "the cursor reaches them");
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
    term.draw(|f| draw(f, &mut app)).unwrap();
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    let pools = app.pools_usd();
    assert!(text.contains(&format!("incl. {} in pools", wallet_core::amount::usd(pools))), "the hero says what it includes");
    let pair = app.home_positions()[1].pair.clone();
    app.selected = tokens + 1;
    app.screen_enter();
    assert_eq!(app.screen, Screen::Pools);
    assert_eq!(app.position_rows()[app.selected].pair, pair, "the same position, under the cursor");
}

/// A picture stays placed when its row is restyled after it was reserved (the selection moving
/// onto the row), and is taken down when something is drawn over its cells.
#[test]
fn a_picture_survives_its_row_being_selected() {
    use super::super::images::{RESERVED, place_inline_icons};
    let (_dir, mut app) = populated_app();
    app.caps.tier = super::super::terminal::Tier::Pixels;
    app.modal = Modal::None;
    let t = app.theme.clone();
    let area = Rect::new(0, 0, 20, 4);
    let picture = Rect::new(2, 1, 4, 2);
    let png: super::super::eco::KittyPng = (std::sync::Arc::new(vec![1, 2, 3]), 7);
    for (restyle, kept) in [(true, true), (false, false)] {
        let mut buf = ratatui::buffer::Buffer::empty(area);
        for y in picture.top()..picture.bottom() {
            for x in picture.left()..picture.right() {
                buf[(x, y)].set_symbol(RESERVED).set_bg(t.surface);
            }
        }
        if restyle {
            // The selection passes over the row: its style changes, its cells do not.
            buf.set_style(Rect::new(0, 1, 20, 1), Style::default().bg(t.selection));
        } else {
            // A popup draws over part of it.
            buf[(3, 2)].set_symbol(" ");
        }
        app.eco.kitty.borrow_mut().push((picture, png.clone(), 0));
        place_inline_icons(&app, &mut buf, &t);
        let placed = super::super::images::kitty_items(&app);
        assert_eq!(placed.len() == 1, kept, "restyled {restyle}: {} placed", placed.len());
    }
}

/// A list longer than its panel shows where you are in it: a thumb on the panel's right border.
#[test]
fn a_long_list_shows_a_scrollbar() {
    let (_dir, mut app) = populated_app();
    app.switch(Screen::Qi);
    let rows = screen_text(&mut app, 100, 30);
    let thumb: usize = rows.iter().filter(|r| r.contains('┃')).count();
    assert!(thumb >= 1, "a thumb on the coin list");
    assert!(thumb < 20, "a thumb, not the whole border: {thumb}");
}

/// The pointer marks what it is over without filling it: a row gets a faint edge, a tab an
/// underline, a shortened address its whole self in a tooltip. Approve never lights.
#[test]
fn hover_marks_without_filling() {
    let (_dir, mut app) = populated_app();
    app.network_id = "mainnet".into();
    app.caps.hyperlinks = true;
    app.switch(Screen::Accounts);
    let draw = |app: &mut App| {
        use ratatui::{Terminal, backend::TestBackend};
        let mut term = Terminal::new(TestBackend::new(100, 30)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        term.backend().buffer().clone()
    };
    let buf = draw(&mut app);
    // The shortened address in the table, and the tab strip.
    let row_of = |needle: &str| (0..30u16).find(|y| (0..100u16).map(|x| buf[(x, *y)].symbol()).collect::<String>().contains(needle));
    let y = row_of("…").expect("a shortened address");
    let line: String = (0..100u16).map(|x| buf[(x, y)].symbol()).collect();
    let x = line.chars().position(|c| c == '…').unwrap() as u16;
    app.pointer.at = Some((x, y));
    let hovered = draw(&mut app);
    let tooltip: String = (0..100u16).map(|cx| hovered[(cx, y + 1)].symbol()).collect();
    let full = app.dash.accounts[0].address.clone();
    assert!(
        tooltip.contains(&full[2..6]) && tooltip.contains(&full[full.len() - 4..]) && tooltip.contains("0x "),
        "the whole address, grouped: {tooltip:?}"
    );
    // A tab under the pointer is underlined, and nothing else about it changes.
    let tabs = row_of("Accounts").unwrap();
    let tab_line: String = (0..100u16).map(|x| buf[(x, tabs)].symbol()).collect();
    let tx = tab_line[..tab_line.find("Qi coins").unwrap()].chars().count() as u16;
    app.pointer.at = Some((tx + 1, tabs));
    let hovered = draw(&mut app);
    assert!(hovered[(tx + 1, tabs)].modifier.contains(Modifier::UNDERLINED));
    assert_eq!(hovered[(tx + 1, tabs)].bg, buf[(tx + 1, tabs)].bg, "no fill");
    app.pointer.at = None;
}

/// The terminal's own cursor waits (hidden) at the focus, for magnifiers and screen readers:
/// the selected row of the list in front. Motion Off stills every spinner.
#[test]
fn the_cursor_waits_at_the_focus_and_off_means_still() {
    let (_dir, mut app) = populated_app();
    app.switch(Screen::Activity);
    app.selected = 0;
    screen_text(&mut app, 100, 30);
    let (x, y) = app.focus_at.expect("the selected row");
    let row = app.hits.borrow().live_regions().iter().find(|(r, _)| r.x == x && r.y == y).map(|(_, t)| t.clone());
    assert!(matches!(row, Some(Target::Row { index: 0, .. })), "{row:?}");
    app.config.motion = Motion::Off;
    app.busy = Some("syncing…".into());
    let rows = screen_text(&mut app, 100, 30);
    assert!(rows[0].contains("◌ syncing"), "a still mark: {:?}", rows[0]);
    assert!(!app.spun, "and no frames asked for it");
}

fn messaging_view(need: wallet_core::messaging::service::KeyNeed) -> super::super::eco::MessagingView {
    use wallet_core::messaging::service::{Conversation, Status};
    use wallet_core::messaging::store::PeerState;
    let convo = |peer: &str, name: Option<&str>, state| Conversation {
        peer: peer.into(),
        name: name.map(Into::into),
        state,
        fingerprint: Some("69ed b347 a6c4 4850 1a6a 92f6 c6c4 fd75".into()),
        verified: false,
        identity_changed: false,
        messages: 2,
        unread: 1,
        last_at: 1,
    };
    super::super::eco::MessagingView {
        status: Status {
            account: Some("0x00a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1".into()),
            need,
            fingerprint: Some("aaaa bbbb cccc dddd eeee ffff 0000 1111".into()),
            key: Some((1, 2959, true)),
            keys_held: 1,
            scanned_to: Some(100),
        },
        conversations: vec![convo("0x00b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0", Some("bob"), PeerState::Accepted)],
        requests: vec![convo("0x00cacacacacacacacacacacacacacacacacacaca", None, PeerState::Request)],
    }
}

/// Private messages sit between the public channels and the old read-only conversations:
/// a set-up row until there is an account, then conversations, then requests. Each draws what
/// it is: what was said, and what needs deciding first.
#[test]
fn private_conversations_and_requests_are_listed_and_drawn() {
    use super::super::eco::BoardRow;
    use wallet_core::messaging::service::{KeyNeed, Line};
    let (_dir, mut app) = drawable_app();
    app.meta.as_mut().unwrap().kind = wallet_core::registry::WalletKind::Hd;
    app.config.board_channels = vec!["general".into()];
    app.eco.board.msg = Some(Ok(messaging_view(KeyNeed::NotSetUp)));
    assert_eq!(app.board_rows(), vec![BoardRow::Channel("general".into()), BoardRow::Messaging]);
    app.switch(Screen::Board);
    app.selected = 1;
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("never backed up") && text.contains("never your main"), "{text}");

    app.eco.board.msg = Some(Ok(messaging_view(KeyNeed::Ready)));
    let bob = "0x00b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0".to_string();
    let carol = "0x00cacacacacacacacacacacacacacacacacacaca".to_string();
    assert_eq!(
        app.board_rows(),
        vec![
            BoardRow::Channel("general".into()),
            BoardRow::Messaging,
            BoardRow::Chat(bob.clone(), Some("bob".into())),
            BoardRow::Request(carol.clone())
        ]
    );
    let line = |outgoing, text: &str, status: &str| Line {
        at: 1,
        outgoing,
        text: text.into(),
        status: status.into(),
        unverified: false,
        tx: String::new(),
    };
    app.eco.board.msg_lines.insert(bob.clone(), Ok(vec![line(false, "meet at nine", "received"), line(true, "see you there", "pending")]));
    app.selected = 2;
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("bob · private"), "{text}");
    assert!(text.contains("meet at nine") && text.contains("see you there   · pending"), "{text}");
    assert!(text.contains("Not verified yet"), "{text}");
    assert!(text.contains("◉ private") && text.contains("◉ requests"), "the groups are headed: {text}");

    app.eco.board.msg_lines.insert(carol.clone(), Ok(vec![line(false, "hello?", "received")]));
    app.selected = 3;
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("request") && text.contains("a accept") && text.contains("Wrote to you first"), "{text}");

    if let Some(Ok(v)) = &mut app.eco.board.msg {
        v.conversations[0].identity_changed = true;
    }
    app.selected = 2;
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("Their identity key changed"), "{text}");
}

/// The messaging account is chosen on the Board: its row opens a list of every account but the
/// main one (and a new one); picking one sets messaging up, and picking another later asks first,
/// since it starts a new identity. The account in use shows its balance and how to fund it.
#[test]
fn the_messaging_account_is_chosen_and_funded_from_the_board() {
    use super::super::eco::BoardRow;
    use crate::tui::app::{ConfirmAction, FormKind};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let press = |app: &mut App, code: KeyCode| app.on_key(KeyEvent::new(code, KeyModifiers::NONE), (160, 48));
    use wallet_core::messaging::service::KeyNeed;
    let (_dir, mut app) = populated_app();
    app.meta.as_mut().unwrap().kind = wallet_core::registry::WalletKind::Hd;
    app.config.board_channels = vec![];
    // A second account, and a third, to choose between.
    for (n, address) in [(2, "0x00b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1"), (3, "0x00c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2c2")] {
        let mut extra = app.dash.accounts[0].clone();
        extra.address = address.into();
        extra.label = format!("Account {n}");
        app.dash.accounts.push(extra);
    }
    app.eco.board.msg = Some(Ok(messaging_view(KeyNeed::NotSetUp)));
    app.switch(Screen::Board);
    let row = app.board_rows().iter().position(|r| *r == BoardRow::Messaging).unwrap();
    app.pane = 0;
    app.selected = row;
    let choices = app.messaging_choices();
    assert_eq!(choices.len(), app.dash.accounts.len(), "every account but the main one, and a new one");
    assert!(choices.iter().all(|(a, _)| a.as_deref() != Some(app.dash.accounts[0].address.as_str())), "never the main one");
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("Choose the account your messages go from") && text.contains("a new account, just for messaging"), "{text}");
    assert!(text.contains("messages go from") && !text.contains("F fund it"), "nothing to fund before there is an account: {text}");
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.pane, 1, "enter goes to the list beside");
    app.eco.board.msg_loading = false;
    press(&mut app, KeyCode::Char('p'));
    assert!(app.eco.board.msg_loading, "picking one sets messaging up");

    // Set up: the pane shows the account and how to fund it; another account asks first.
    let mut view = messaging_view(KeyNeed::Ready);
    view.status.account = Some(app.dash.accounts[1].address.clone());
    app.eco.board.msg = Some(Ok(view));
    app.eco.board.msg_loading = false;
    app.pane = 0;
    app.selected = app.board_rows().iter().position(|r| *r == BoardRow::Messaging).unwrap();
    let text = screen_text(&mut app, 160, 48).join("\n");
    assert!(text.contains("F fund it") && text.contains("move messaging to") && text.contains(" ✓ "), "{text}");
    press(&mut app, KeyCode::Char('p'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('p'));
    assert!(matches!(&app.modal, Modal::Confirm { action: ConfirmAction::MoveMessaging(_), .. }), "moving asks first");
    app.modal = Modal::None;
    press(&mut app, KeyCode::Char('F'));
    assert!(matches!(&app.modal, Modal::Form(f) if f.kind == FormKind::MessagingFund), "F funds it from the Board");
}
