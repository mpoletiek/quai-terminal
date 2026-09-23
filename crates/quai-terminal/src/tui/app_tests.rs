use super::*;
use wallet_core::sdk::U256;

#[test]
fn palette_fuzzy() {
    let (_dir, app) = test_app(WalletKind::Hd);
    let m = app.palette_entries("cnv qi");
    assert!(m.iter().any(|e| matches!(e.run, super::super::palette::Run::Action(id) if id.starts_with("convert"))));
    assert_eq!(app.palette_entries("").len(), ACTIONS.len(), "empty: every action, nothing recent yet");
    assert!(matches!(app.palette_entries("theme").first().map(|e| &e.run), Some(super::super::palette::Run::Action("themes"))));
    // Screens are there too, and what words mean.
    assert!(app.palette_entries("markets").iter().any(|e| e.run == super::super::palette::Run::Go(Screen::Markets)));
    let term = app.palette_entries("what is slippage").into_iter().next().unwrap();
    assert_eq!(term.tag, "term");
    let mut app = app;
    app.run_palette(term);
    assert!(matches!(app.modal, Modal::Glossary { selected } if super::super::glossary::TERMS[selected].word == "slippage"));
}

/// "send alice 5 quai" opens a filled send form; nothing is signed until the review.
#[test]
fn palette_reads_a_send_and_fills_the_form() {
    use super::super::palette::Run;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.contacts = vec![wallet_core::appdb::Contact {
        id: 1,
        name: "Alice".into(),
        address: Some("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into()),
        payment_code: None,
        note: String::new(),
    }];
    for q in ["send alice 5 quai", "send 5 quai to alice", "pay ali 5"] {
        let first = app.palette_entries(q).into_iter().next().unwrap();
        assert_eq!(first.tag, "do", "{q}");
        assert_eq!(first.label, "Send 5 QUAI to Alice", "{q}");
        assert!(matches!(&first.run, Run::Send { kind: FormKind::SendQuai, to, amount, .. } if to == "Alice" && amount == "5"));
    }
    let qi = app.palette_entries("send bob 2 qi").into_iter().next().unwrap();
    assert!(matches!(qi.run, Run::Send { kind: FormKind::SendQi, .. }));
    let entry = app.palette_entries("send alice 5 quai").into_iter().next().unwrap();
    app.run_palette(entry);
    let Modal::Form(f) = &app.modal else { panic!("a send form opens") };
    assert_eq!(f.kind, FormKind::SendQuai);
    assert_eq!(f.fields.iter().find(|fl| fl.label == "To").unwrap().value, "Alice");
    assert_eq!(f.fields.iter().find(|fl| fl.label == "Amount").unwrap().value, "5");
    // And it is remembered, first among recents, across a reopen.
    app.modal = Modal::None;
    app.palette_recent.clear();
    app.open_palette();
    assert_eq!(app.palette_recent.first().map(String::as_str), Some("do:Send 5 QUAI to Alice"));
}

/// "swap 10 wqi to usdt" sets the swap card's pair and amount.
#[test]
fn palette_reads_a_swap() {
    use super::super::palette::Run;
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let first = app.palette_entries("swap 10 smol to quai").into_iter().next().unwrap();
    assert_eq!(first.label, "Swap 10 SMOL → QUAI");
    app.run_palette(first);
    assert_eq!(app.screen, Screen::Swap);
    assert_eq!(app.eco.swap.amount, "10");
    assert_eq!(app.eco.swap.from.symbol(), "SMOL");
    assert_eq!(app.eco.swap.to, Some(SwapAsset::Quai));
    // Buying names what arrives, paid in QUAI unless said otherwise.
    let buy = app.palette_entries("buy smol").into_iter().next().unwrap();
    assert!(matches!(&buy.run, Run::Swap { from: SwapAsset::Quai, to: Some(t), .. } if t.symbol() == "SMOL"));
}

#[test]
fn jump_labels_roundtrip() {
    for i in 0..36 {
        assert_eq!(label_index(jump_label(i)), Some(i));
    }
}

fn form(kind: FormKind, fields: Vec<Field>) -> Form {
    Form { kind, title: String::new(), fields, focus: 0, note: None, contract_note: None, pending: false, error: None, error_field: None }
}

#[test]
fn validation_points_at_fields() {
    let f = form(FormKind::SendQi, vec![Field::new("To", "").with("bob"), Field::new("Amount", "").amount("QI").with("1.2.3")]);
    assert_eq!(validate(&f).unwrap_err().0, 1);
    let f = form(FormKind::Approve, vec![Field::new("Amount", "")]);
    assert!(validate(&f).is_err(), "approve amount must be explicit");
    let f = form(FormKind::Approve, vec![Field::new("Amount", "").with("unlimited")]);
    assert!(validate(&f).is_ok());
    let f = form(FormKind::ConvertQuaiToQi, vec![Field::new("Slippage", "").with("20000")]);
    assert!(validate(&f).is_err());
    let f = form(FormKind::SendQuai, vec![Field::new("Max fee", "").optional()]);
    assert!(validate(&f).is_ok());
    let e = form(FormKind::SendQuai, vec![Field::new("To", ""), Field::new("Amount", "").amount("QUAI")]);
    assert_eq!(error_field(&e, "insufficient balance for this amount plus the fee"), Some(1));
}

#[test]
fn choices_cycle() {
    let mut f = Field::new("Direction", "").choice(vec![("a".into(), "A".into()), ("b".into(), "B".into())]);
    assert_eq!(f.value, "a");
    f.cycle(1);
    assert_eq!(f.choice_label(), Some("B"));
    f.cycle(1);
    assert_eq!(f.value, "a");
}

#[test]
fn strength_and_paths() {
    assert_eq!(password_strength("short"), 0);
    assert!(password_strength("correct-Horse-battery-9") >= 3);
    assert!(short_path("/a/very/long/path/that/goes/on/and/on/forever/and/ever/amen").chars().count() <= 40);
}

#[test]
fn review_requires_scroll() {
    let review = Review {
        op_id: "x".into(),
        kind: "send_quai".into(),
        title: "t".into(),
        network: "n".into(),
        from: "a".into(),
        to: "b".into(),
        asset: "QUAI".into(),
        amount: "1".into(),
        amount_base: "1".into(),
        max_fee: "1".into(),
        fee_bps: None,
        fields: vec![],
        coins: vec![],
        warnings: vec![],
        visuals: vec![],
        fee_over_policy: false,
        changes: vec![],
    };
    let mut r = ReviewState {
        review,
        scroll: 0,
        content_lines: 40,
        viewport: 10,
        approve_focused: true,
        opened: Instant::now() - std::time::Duration::from_secs(2),
    };
    assert!(!r.can_approve());
    r.scroll = 30;
    assert!(r.can_approve());
    assert!((r.read_ratio() - 1.0).abs() < f64::EPSILON);
}

fn test_app(kind: WalletKind) -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let mut meta = registry.create_watch("t", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    meta.kind = kind;
    let caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Cells);
    // Every feature on, so each test reaches the screen it is about; the switches have their own.
    let config = AppConfig { features: wallet_core::config::Features { messaging: true, trading: true, nfts: true }, ..Default::default() };
    let mut app = App::new(paths, "local".into(), config, Theme::terminal(false), caps, Some(meta));
    app.locked = false;
    app.onboarding = None;
    (dir, app)
}

/// A sync ending while a transaction is prepared must not take its spinner away: the signing
/// lane's status stays up until that lane itself is done, and outranks the worker's.
#[test]
fn preparing_spinner_survives_a_sync_finishing() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let size = (100, 30);
    app.on_event(Ev::Busy(Some("syncing…".into())), size);
    app.on_event(Ev::SignBusy(Some("preparing transaction…".into())), size);
    assert_eq!(app.busy_label(), Some("preparing transaction…"));
    app.on_event(Ev::Busy(None), size);
    app.on_event(Ev::Busy(Some("checking limit orders…".into())), size);
    app.on_event(Ev::Busy(None), size);
    assert_eq!(app.busy_label(), Some("preparing transaction…"));
    app.on_event(Ev::SignBusy(None), size);
    assert_eq!(app.busy_label(), None);
}

/// First run opens on a welcome, not a question; the look step then asks about motion, which
/// plays as the cursor moves, is kept on enter, and is put back on esc.
#[test]
fn onboarding_welcomes_then_asks_how_much_should_move() {
    use super::super::onboarding;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use wallet_core::config::Motion;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let press = |app: &mut App, code: KeyCode| onboarding::on_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    app.meta = None;
    app.onboarding = None;
    app.start_onboarding();
    assert!(matches!(app.onboarding, Some(Onboarding::Welcome)));
    assert_eq!(onboarding::step(app.onboarding.as_ref().unwrap()), 0, "the welcome is before the steps");
    press(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.onboarding, Some(Onboarding::Welcome)), "only enter begins");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.onboarding, Some(Onboarding::Theme(_))));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.onboarding, Some(Onboarding::Motion { selected: 0, from: Motion::Vivid })), "cursor on what is set");
    assert_eq!(onboarding::step(app.onboarding.as_ref().unwrap()), 1, "motion is part of the look");
    // Moving previews; esc puts the old one back and returns to the themes.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.config.motion, Motion::Reduced, "a preview under the cursor");
    press(&mut app, KeyCode::Esc);
    assert_eq!(app.config.motion, Motion::Vivid, "esc restores");
    assert!(matches!(app.onboarding, Some(Onboarding::Theme(_))));
    // Enter keeps it and moves on to privacy.
    press(&mut app, KeyCode::Esc);
    for _ in 0..5 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.config.motion, Motion::Off, "the list stops at its end");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.config.motion, Motion::Off);
    assert!(matches!(app.onboarding, Some(Onboarding::Privacy { .. })));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.onboarding, Some(Onboarding::Motion { selected: 3, from: Motion::Off })), "back lands on the choice");
}

/// Nothing address-linked is on before the user chooses, private is the preselected choice,
/// and the choice is saved — the explorer only learns addresses the user agreed to share.
#[test]
fn onboarding_asks_about_privacy_and_defaults_to_private() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    assert!(!app.config.explorer_lookups && !app.config.images && !app.config.token_icons, "private until asked");
    let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
    app.onboarding = Some(Onboarding::Privacy { selected: 0 });
    super::super::onboarding::on_key(&mut app, key(KeyCode::Enter));
    assert!(matches!(app.onboarding, Some(Onboarding::Connections { .. })), "on to the connections step");
    assert!(!app.config.explorer_lookups && app.config.data_disclosure_shown);
    // Going back reopens the question where the user left it; choosing connected turns it on.
    super::super::onboarding::on_key(&mut app, key(KeyCode::Esc));
    assert!(matches!(app.onboarding, Some(Onboarding::Privacy { selected: 0 })));
    super::super::onboarding::on_key(&mut app, key(KeyCode::Down));
    super::super::onboarding::on_key(&mut app, key(KeyCode::Enter));
    assert!(app.config.explorer_lookups && app.config.images && app.config.token_icons);
    app.flush_config();
    let saved = AppConfig::load(&app.paths).unwrap();
    assert!(saved.explorer_lookups, "the choice is written to config.toml");
}

/// Adding a wallet from System › Wallets can always be left with Esc. It used to walk back into
/// first-run setup (connections, privacy, theme) and loop there.
#[test]
fn adding_a_wallet_can_be_left_with_esc() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    for kind in [OnboardKind::Create, OnboardKind::ImportPhrase, OnboardKind::ImportKey, OnboardKind::Watch] {
        app.begin_onboarding(kind.clone());
        assert!(app.onboarding.is_some());
        super::super::onboarding::on_key(&mut app, esc);
        assert!(app.onboarding.is_none(), "{kind:?}: one Esc leaves");
    }
    // First run still steps back to the choice of kind.
    app.meta = None;
    app.begin_onboarding(OnboardKind::Watch);
    super::super::onboarding::on_key(&mut app, esc);
    assert!(matches!(app.onboarding, Some(Onboarding::Choose { .. })));
}

/// A portfolio row with every field set, for tests that only care about a couple of them.
fn asset_row(key: wallet_core::portfolio::AssetKey, symbol: &str, balance: &str, exact: bool) -> wallet_core::portfolio::AssetRow {
    wallet_core::portfolio::AssetRow {
        key,
        symbol: symbol.into(),
        name: symbol.into(),
        balance: balance.into(),
        decimals: 18,
        exact,
        price_usd: None,
        price_kind: wallet_core::portfolio::PriceKind::None,
        price_source: String::new(),
        price_at: 0,
        value_usd: None,
        allocation: 0.0,
        change_24h: None,
        icon_url: None,
        trust: wallet_core::portfolio::Trust::Verified,
        holders: None,
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE), (100, 30));
}

/// An item from the action sheet: space, then its letter.
fn sheet(app: &mut App, c: char) {
    press(app, KeyCode::Char(' '));
    assert!(matches!(app.modal, Modal::Sheet { .. }), "space opens the action sheet");
    press(app, KeyCode::Char(c));
}

fn ctrl(app: &mut App, c: char) {
    app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL), (100, 30));
}

fn op(id: &str, kind: &str, status: wallet_core::appdb::OpStatus) -> wallet_core::appdb::Operation {
    wallet_core::appdb::Operation {
        id: id.into(),
        network: "local".into(),
        kind: kind.into(),
        store: "quai".into(),
        account: "0x00".into(),
        status,
        tx_hash: None,
        asset: "WQI".into(),
        amount: "1".into(),
        counterparty: String::new(),
        fee: "0".into(),
        detail: serde_json::json!({}),
        created: 0,
        updated: 0,
    }
}

#[test]
fn listing_runs_as_a_sequence_and_cancel_is_one_step() {
    use super::super::eco::FlowKind;
    use wallet_core::explorer::{NftItem, TokenKind};
    use wallet_core::market::OwnedNft;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let item = |id: &str, kind: TokenKind| OwnedNft {
        item: NftItem {
            contract: "0x0046e5085a830567f647fe52672926bedc8d5c55".into(),
            token_id: id.into(),
            name: format!("SQUID {id}"),
            ..NftItem::default()
        },
        owner: "0x004dd9afaa2768642b5cde15c24f37bf19d842e4".into(),
        kind,
        quantity: "1".into(),
        verified: true,
    };
    app.eco.nfts = Some(Ok(vec![item("224", TokenKind::Erc721), item("5", TokenKind::Erc1155)]));
    app.open_nft_list("0x0046e5085a830567f647fe52672926bedc8d5c55", "5");
    assert!(matches!(app.modal, Modal::None), "ERC-1155 items are not listed on Zora asks");
    app.open_nft_list("0x0046e5085a830567f647fe52672926bedc8d5c55", "224");
    let Modal::Form(mut form) = std::mem::replace(&mut app.modal, Modal::None) else { panic!("list form") };
    assert!(matches!(form.kind, FormKind::NftList { .. }));
    form.fields[0].value = "250".into();
    form.fields[1].value = "WQI".into();
    app.submit_form(&form);
    match app.eco.flow.as_ref().map(|f| f.kind.clone()) {
        Some(FlowKind::NftList { price, currency, account, .. }) => {
            assert_eq!((price.as_deref(), currency.as_str()), (Some("250"), "WQI"));
            assert_eq!(account.as_deref(), Some("0x004dd9afaa2768642b5cde15c24f37bf19d842e4"), "the holding account lists it");
        }
        other => panic!("listing flow: {other:?}"),
    }
    app.eco.flow = None;
    app.cancel_nft_listing("0x0046e5085a830567f647fe52672926bedc8d5c55", "224");
    assert!(matches!(app.eco.flow.as_ref().map(|f| &f.kind), Some(FlowKind::NftList { price: None, .. })));
}

#[test]
fn confirmations_and_coins_light_once() {
    use wallet_core::appdb::OpStatus;
    use wallet_core::session::{CoinView, QiSummary};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let health = |height: u64| wallet_core::network::NodeHealth {
        network: "local".into(),
        chain_id: "1".into(),
        genesis: "0x".into(),
        identity_ok: true,
        height,
        head_hash: format!("0x{height:064x}"),
        head_age_secs: None,
        gas_price: "0".into(),
        client_version: None,
        latency_ms: 1,
        order: Some(if height.is_multiple_of(2) { 0 } else { 2 }),
    };
    let coin = |outpoint: &str, denomination: u8| CoinView {
        outpoint: outpoint.into(),
        address: "0x00".into(),
        qits: 1,
        denomination,
        unlock_height: Default::default(),
        reserved: false,
        origin: "receive".into(),
        peer: None,
        label: None,
    };
    let mut mined = op("aa", "send_quai", OpStatus::Confirmed);
    mined.detail = serde_json::json!({"included_block": 100});
    let mut dash = app.dash.clone();
    dash.network_id = "local".into();
    dash.refreshed_at = 1;
    dash.health = Some(health(101));
    dash.ops = vec![mined.clone()];
    dash.qi = Some(QiSummary { balance: Default::default(), checkpoint_height: None, coins: vec![coin("a:0", 3)] });
    app.dash = dash.clone();
    // Head 101 → 104: 5 confirmations reached; a new coin of denomination 5 arrives.
    let mut next = dash.clone();
    next.health = Some(health(104));
    next.qi = Some(QiSummary { balance: Default::default(), checkpoint_height: None, coins: vec![coin("a:0", 3), coin("b:1", 5)] });
    app.observe_changes(&next);
    assert!(app.row_flash.contains_key("aa"), "row lit at the confirmation target");
    assert_eq!(app.drawer_flash.keys().copied().collect::<Vec<_>>(), vec![5], "only the new coin's slot lights");
    assert_eq!((app.beat_order, app.recent_hashes.len()), (0, 1), "prime block heartbeat and hash history");
    assert!(app.beat.is_some());
}

fn review(id: &str, kind: &str) -> Ev {
    Ev::Review(Box::new(Review {
        op_id: id.into(),
        kind: kind.into(),
        title: "t".into(),
        network: "n".into(),
        from: "a".into(),
        to: "b".into(),
        asset: "WQI".into(),
        amount: "1".into(),
        amount_base: "1".into(),
        max_fee: "1".into(),
        fee_bps: None,
        fields: vec![],
        coins: vec![],
        warnings: vec![],
        visuals: vec![],
        fee_over_policy: false,
        changes: vec![],
    }))
}

fn submitted(id: &str) -> Ev {
    Ev::Submitted(wallet_core::tx::Submitted {
        op_id: id.into(),
        tx_hash: "0x".into(),
        status: wallet_core::appdb::OpStatus::Submitted,
        explorer: None,
        message: String::new(),
    })
}

/// A review armed and read, then hidden by a window shrunk below the minimum, cannot be approved:
/// nothing is drawn there, so Enter is held back. Esc still rejects, because backing out is safe.
#[test]
fn a_review_hidden_by_a_too_small_window_does_not_sign() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    app.on_event(review("hidden", "send"), (100, 30));
    let Modal::Review(r) = &mut app.modal else { panic!("review open") };
    r.approve_focused = true;
    r.content_lines = 10;
    r.viewport = 10;
    r.opened = Instant::now() - std::time::Duration::from_secs(2);
    assert!(r.can_approve());
    let tiny = (40, 12);
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), tiny);
    assert!(matches!(app.modal, Modal::Review(_)), "nothing signed while the review can't be seen");
    app.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), tiny);
    let Modal::Review(r) = &app.modal else { panic!("still open") };
    assert!(r.approve_focused, "no key reaches the hidden review");
    app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), tiny);
    assert!(matches!(app.modal, Modal::None), "Esc rejects even when too small to draw");
}

/// A form that finishes on this computer closes when it is submitted. It must never be left
/// waiting on the worker, which would sit on "preparing…" for a review that never comes.
#[test]
fn a_form_that_finishes_here_closes_instead_of_waiting_for_a_review() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.load_wallets();
    let wallet = app.wallets.first().cloned().expect("the test wallet");
    assert!(FormKind::RenameWallet(wallet.id.clone()).is_local(), "renaming never reaches the worker");
    app.open_form(FormKind::RenameWallet(wallet.id.clone()));
    // The field arrives holding the current name; add to it and submit.
    press(&mut app, KeyCode::Char('2'));
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Modal::None), "the form closed");
    app.load_wallets();
    let renamed = app.wallets.iter().find(|w| w.id == wallet.id).expect("still there");
    assert_eq!(renamed.name, format!("{}2", wallet.name));
    // A form that does reach the worker is not treated as local.
    assert!(!FormKind::SendQuai.is_local());
    assert!(FormKind::FollowChannel.is_local(), "following a channel is a local preference");
}

#[test]
fn swap_sequence_waits_for_the_approval_then_opens_the_swap_on_any_screen() {
    use super::super::eco::FlowKind;
    use wallet_core::appdb::OpStatus;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    let size = (100, 30);
    app.start_flow(FlowKind::Swap {
        account: None,
        from: "0x002b".into(),
        to: "0x0049".into(),
        amount: "1".into(),
        slippage: 50,
        deadline: 10,
        label: "swap 1 WQI → USDT".into(),
        prewrap: None,
        unwrap_after: false,
        baseline: "0".into(),
        then: None,
    });
    assert!(app.eco.flow.as_ref().unwrap().requested, "first review requested");
    // Step 1: the approval review arrives, is approved and submitted.
    app.on_event(review("a1", "approve"), size);
    assert!(matches!(app.modal, Modal::Review(_)));
    app.modal = Modal::None;
    app.committing_kind = Some("approve".into());
    app.on_event(submitted("a1"), size);
    assert!(matches!(app.modal, Modal::None), "an intermediate step shows no result dialog");
    assert_eq!(app.eco.flow.as_ref().unwrap().waiting.as_deref(), Some("a1"));
    // The user wanders off; nothing is requested while the approval is unconfirmed.
    app.switch_section(Section::Nfts);
    app.dash.ops = vec![op("a1", "approve", OpStatus::Submitted)];
    app.advance_flow();
    assert!(!app.eco.flow.as_ref().unwrap().requested);
    // Confirmation: the swap review is requested wherever the user is.
    app.dash.ops = vec![op("a1", "approve", OpStatus::Confirmed)];
    app.advance_flow();
    let flow = app.eco.flow.as_ref().unwrap();
    assert!(flow.requested && flow.waiting.is_none());
    app.on_event(review("s1", "swap"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    app.on_event(submitted("s1"), size);
    assert!(app.eco.flow.is_none(), "the swap finishes the sequence");
    assert!(matches!(app.modal, Modal::Result(_)), "the final step shows its result");
}

/// A deposit is three reviews — an exact approval per side, then the add — and each one has
/// to be asked for again after the last confirms. Firing one review and stopping is how an
/// approval goes through and nothing else ever appears.
#[test]
fn a_deposit_walks_both_approvals_and_then_deposits() {
    use super::super::eco::FlowKind;
    use super::super::worker::Prepare;
    use wallet_core::appdb::OpStatus;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    let size = (100, 30);
    let prepare = Prepare::AddLiquidityNext {
        account: None,
        pair: "0x00pair".into(),
        amount: "3.5".into(),
        token: Some("WQI".into()),
        slippage: 50,
        deadline: 10,
    };
    app.start_flow(FlowKind::Steps { prepare: Box::new(prepare.clone()), label: "add liquidity to SMOL/WQI".into() });
    assert!(app.eco.flow.as_ref().unwrap().requested, "the first review is asked for");
    // Each approval: reviewed, submitted, then waited on before the next is requested.
    for (i, id) in ["ap0", "ap1"].iter().enumerate() {
        app.on_event(review(id, "approve"), size);
        assert!(matches!(app.modal, Modal::Review(_)), "approval {i} opens a review");
        app.modal = Modal::None;
        app.committing_kind = Some("approve".into());
        app.on_event(submitted(id), size);
        assert_eq!(app.eco.flow.as_ref().unwrap().waiting.as_deref(), Some(*id), "waits for approval {i}");
        app.dash.ops = vec![op(id, "approve", OpStatus::Submitted)];
        app.advance_flow();
        assert!(!app.eco.flow.as_ref().unwrap().requested, "nothing asked while approval {i} is unconfirmed");
        app.dash.ops = vec![op(id, "approve", OpStatus::Confirmed)];
        app.advance_flow();
        let flow = app.eco.flow.as_ref().expect("the sequence continues past the approval");
        assert!(flow.requested && flow.waiting.is_none(), "the next step is asked for after approval {i}");
    }
    // The deposit itself ends the sequence and shows its result.
    app.on_event(review("add1", "add_liquidity"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("add_liquidity".into());
    app.on_event(submitted("add1"), size);
    assert!(app.eco.flow.is_none(), "the deposit finishes the sequence");
    assert!(matches!(app.modal, Modal::Result(_)), "and shows its result");
}

/// A deposit short of WQUAI starts by wrapping the shortfall from QUAI. That wrap is a step like
/// an approval — waited on, then the sequence asks again — never the end of the deposit.
#[test]
fn a_deposit_short_of_wquai_wraps_then_approves_then_deposits() {
    use super::super::eco::FlowKind;
    use super::super::worker::Prepare;
    use wallet_core::appdb::OpStatus;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    let size = (100, 30);
    let prepare = Prepare::AddLiquidityNext {
        account: None,
        pair: "0x00pair".into(),
        amount: "3.5".into(),
        token: Some("WQUAI".into()),
        slippage: 50,
        deadline: 10,
    };
    app.start_flow(FlowKind::Steps { prepare: Box::new(prepare), label: "add liquidity to SMOL/WQUAI".into() });
    for (id, kind) in [("wrap0", "wrap_quai"), ("ap0", "approve")] {
        app.on_event(review(id, kind), size);
        assert!(matches!(app.modal, Modal::Review(_)), "{kind} opens a review");
        app.modal = Modal::None;
        app.committing_kind = Some(kind.into());
        app.on_event(submitted(id), size);
        let flow = app.eco.flow.as_ref().unwrap_or_else(|| panic!("the {kind} does not end the deposit"));
        assert_eq!(flow.waiting.as_deref(), Some(id), "the sequence waits for the {kind}");
        assert!(!matches!(app.modal, Modal::Result(_)), "and shows no final result for it");
        app.dash.ops = vec![op(id, kind, OpStatus::Submitted)];
        app.advance_flow();
        assert!(!app.eco.flow.as_ref().unwrap().requested, "nothing asked while the {kind} is unconfirmed");
        app.dash.ops = vec![op(id, kind, OpStatus::Confirmed)];
        app.advance_flow();
        let flow = app.eco.flow.as_ref().expect("the sequence continues");
        assert!(flow.requested && flow.waiting.is_none(), "the next step is asked for after the {kind}");
    }
    app.on_event(review("add1", "add_liquidity"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("add_liquidity".into());
    app.on_event(submitted("add1"), size);
    assert!(app.eco.flow.is_none(), "the deposit finishes the sequence");
    assert!(matches!(app.modal, Modal::Result(_)), "and shows its result");
}

/// The market route is a sequence of reviewed steps: wrap, swap, then redeem exactly what
/// the swap produced.
#[test]
fn qi_market_route_runs_wrap_swap_unwrap() {
    use super::super::eco::FlowKind;
    use wallet_core::appdb::OpStatus;
    use wallet_core::qi_market::Direction;
    use wallet_core::sdk::U256;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.network_id = "mainnet".into();
    app.dash.unlocked = true;
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::from(100u64) * U256::from(10u64).pow(U256::from(18)),
        locked: U256::ZERO,
        nonce: 0,
    }];
    let wrap = |wqi: &str, wquai: &str| {
        Some(wallet_core::ops::WrapStatus {
            account: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
            wqi_atoms: Some(wqi.into()),
            wqi_qi: None,
            unclaimed_qits: None,
            wquai_atoms: Some(wquai.into()),
        })
    };
    app.dash.wrap = wrap("0", "0");
    let size = (100, 30);
    app.start_qi_route(Direction::QuaiToQi, "10".into(), 50);
    // The route is one durable core intent, not a TUI-held stage machine: QUAI buys WQI on the
    // market, and the redemption that follows is sized from that swap's own receipt.
    let wqi = app.config.network("mainnet").unwrap().wqi.unwrap();
    match &app.eco.flow.as_ref().expect("the route started").kind {
        FlowKind::Steps { prepare, .. } => {
            let Prepare::Trading { intent } = prepare.as_ref() else { panic!("{prepare:?}") };
            assert!(
                matches!(
                    &intent.action,
                    wallet_core::execution::TradingAction::MarketConversion { direction: Direction::QuaiToQi, amount, stage: 0, .. }
                        if amount == "10"
                ),
                "{:?}",
                intent.action
            );
        }
        other => panic!("{other:?}"),
    }
    // The market swap is signed: the route waits for it rather than announcing a result.
    app.on_event(review("s1", "swap"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    app.on_event(submitted("s1"), size);
    assert!(matches!(app.modal, Modal::None), "an intermediate step shows no result dialog");
    assert_eq!(app.eco.flow.as_ref().unwrap().waiting.as_deref(), Some("s1"));
    // 8.285 WQI arrives: whole Qi are redeemed and the remainder is recorded, not silently kept.
    let mut paid = op("s1", "swap", OpStatus::Confirmed);
    paid.account = "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into();
    paid.detail = serde_json::json!({"to_token": wqi, "actual_out": "8285000000000000000"});
    app.dash.ops = vec![paid];
    app.dash.wrap = wrap("8285000000000000000", "0");
    app.advance_flow();
    match &app.eco.flow.as_ref().expect("the redemption is next").kind {
        FlowKind::Steps { prepare, .. } => {
            let Prepare::Trading { intent } = prepare.as_ref() else { panic!("{prepare:?}") };
            assert!(
                matches!(
                    &intent.action,
                    wallet_core::execution::TradingAction::MarketConversion { stage: 1, amount, residual_atoms, .. }
                        if amount == "8" && residual_atoms == "285000000000000000"
                ),
                "{:?}",
                intent.action
            );
        }
        other => panic!("{other:?}"),
    }
    // The redemption is the last allocation, so it ends the route and shows its result.
    app.on_event(review("u1", "unwrap_wqi"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("unwrap_wqi".into());
    app.on_event(submitted("u1"), size);
    assert!(app.eco.flow.is_none(), "the redemption ends the route");
    assert!(matches!(app.modal, Modal::Result(_)), "the final step shows its result");
}

/// A WQUAI pool can be paid from QUAI: the sequence wraps what is missing first, and offers
/// to redeem WQUAI it pays out.
#[test]
fn swapping_a_wquai_pool_wraps_in_and_redeems_out() {
    use super::super::eco::FlowKind;
    use wallet_core::sdk::U256;
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.network_id = "mainnet".into();
    app.dash.unlocked = true;
    let wquai = app.config.network("mainnet").unwrap().wquai.unwrap().to_lowercase();
    let usdt = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".to_string();
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::from(100u64) * U256::from(10u64).pow(U256::from(18)),
        locked: U256::ZERO,
        nonce: 0,
    }];
    let quote = |from: &SwapAsset, to: &SwapAsset| wallet_core::swap::SwapQuote {
        from: from.clone(),
        to: to.clone(),
        amount_in: "10000000000000000000".into(),
        amount_out: "1".into(),
        minimum_out: "1".into(),
        slippage_bps: 50,
        path: vec![],
        route: vec![],
        pools: vec![],
        impact_bps: 0,
        fee_bps: 30,
        router: "0x00".into(),
        allowance: None,
        approval_needed: false,
        balance: Some("0".into()),
        // No WQUAI held: the QUAI balance has to cover it.
        insufficient: true,
        warnings: vec![],
        observed_at: 0,
        liquidity_at: None,
        legs: vec![],
    };
    let wquai_asset = SwapAsset::Token { address: wquai.clone(), symbol: "WQUAI".into(), decimals: 18 };
    let usdt_asset = SwapAsset::Token { address: usdt, symbol: "USDT".into(), decimals: 6 };
    app.eco.swap.from = wquai_asset.clone();
    app.eco.swap.to = Some(usdt_asset.clone());
    app.eco.swap.amount = "10".into();
    app.eco.swap.quote = Some(Ok(quote(&wquai_asset, &usdt_asset)));
    app.eco.swap.quote_key = 1;
    app.eco.swap.requested_key = 1;
    app.eco.swap.requested_input = app.swap_input_key();
    app.eco.swap.quoted_at = Some(Instant::now());
    app.swap_submit();
    match &app.eco.flow.as_ref().expect("sequence started").kind {
        FlowKind::Swap { prewrap, unwrap_after, .. } => {
            assert_eq!(prewrap.as_deref(), Some("10"), "wraps the missing WQUAI first");
            assert!(!unwrap_after, "the output is USDT");
        }
        other => panic!("{other:?}"),
    }
    // Paying with USDT for WQUAI: nothing to wrap, but the output can be redeemed.
    app.eco.flow = None;
    app.eco.swap.from = usdt_asset.clone();
    app.eco.swap.to = Some(wquai_asset.clone());
    let mut q = quote(&usdt_asset, &wquai_asset);
    q.insufficient = false;
    app.eco.swap.quote = Some(Ok(q));
    app.eco.swap.requested_input = app.swap_input_key();
    app.swap_submit();
    match &app.eco.flow.as_ref().expect("sequence started").kind {
        FlowKind::Swap { prewrap, unwrap_after, .. } => {
            assert!(prewrap.is_none() && *unwrap_after, "redeems the WQUAI it pays out");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn sequences_stop_on_reject_failure_and_errors() {
    use super::super::eco::FlowKind;
    use wallet_core::appdb::OpStatus;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    let size = (100, 30);
    let swap = || FlowKind::Swap {
        account: None,
        from: "0x002b".into(),
        to: "0x0049".into(),
        amount: "1".into(),
        slippage: 50,
        deadline: 10,
        label: "swap".into(),
        prewrap: None,
        unwrap_after: false,
        baseline: "0".into(),
        then: None,
    };
    // Rejecting a step's review ends the sequence.
    app.start_flow(swap());
    app.on_event(review("a1", "approve"), size);
    press(&mut app, KeyCode::Esc);
    assert!(app.eco.flow.is_none());
    // A failed approval ends it.
    app.start_flow(swap());
    app.on_event(review("a2", "approve"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("approve".into());
    app.on_event(submitted("a2"), size);
    app.dash.ops = vec![op("a2", "approve", OpStatus::Failed)];
    app.advance_flow();
    assert!(app.eco.flow.is_none());
    // A preparation error (e.g. not enough balance) ends it.
    app.start_flow(swap());
    app.on_event(Ev::Error("WQI balance is 0".into()), size);
    assert!(app.eco.flow.is_none());
    // Locking drops the open review; the step is requested again after unlocking.
    app.start_flow(swap());
    app.on_event(review("a3", "approve"), size);
    app.modal = Modal::None;
    app.flow_on_lock();
    app.advance_flow();
    assert!(app.eco.flow.as_ref().unwrap().requested);
}

#[test]
fn settled_wrapped_qi_prompts_one_claim_review_until_rejected() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    let size = (100, 30);
    app.dash.wrap = Some(wallet_core::ops::WrapStatus {
        account: "0x00".into(),
        wqi_atoms: Some("0".into()),
        wqi_qi: Some("0".into()),
        unclaimed_qits: Some("1000".into()),
        wquai_atoms: None,
    });
    app.tick_eco();
    assert!(matches!(app.eco.flow.as_ref().map(|f| &f.kind), Some(super::super::eco::FlowKind::Claim { .. })));
    app.on_event(review("c1", "claim_wqi"), size);
    press(&mut app, KeyCode::Esc);
    assert!(app.eco.flow.is_none());
    app.tick_eco();
    assert!(app.eco.flow.is_none(), "a rejected claim is not prompted again for the same amount");
    // More backing arrives: prompt again.
    app.dash.wrap.as_mut().unwrap().unclaimed_qits = Some("2500".into());
    app.tick_eco();
    assert!(app.eco.flow.is_some());
    // Watch-only and locked wallets are never prompted.
    let (_d2, mut watch) = test_app(WalletKind::Watch);
    watch.dash.wrap = app.dash.wrap.clone();
    watch.dash.unlocked = true;
    watch.tick_eco();
    assert!(watch.eco.flow.is_none());
}

#[test]
fn lock_key_locks_immediately_and_worker_confirmation_is_idempotent() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    press(&mut app, KeyCode::Char('l'));
    assert!(!app.locked, "l moves right; it no longer locks");
    ctrl(&mut app, 'l');
    assert!(app.locked, "one press locks without waiting for the worker");
    assert!(!app.dash.unlocked);
    // A refresh that was in flight must not reveal unlocked data again.
    app.on_event(Ev::Dashboard(Box::new(Dashboard { unlocked: true, ..Dashboard::default() })), (100, 30));
    assert!(!app.dash.unlocked);
    app.on_event(Ev::Locked, (100, 30));
    assert!(app.locked);
    // Watch-only wallets have nothing to lock.
    let (_dir, mut watch) = test_app(WalletKind::Watch);
    press(&mut watch, KeyCode::Char('l'));
    assert!(!watch.locked);
}

#[test]
fn channels_tab_saves_a_channel_as_contact() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Contacts);
    app.dash.unlocked = true;
    app.dash.peers = vec![wallet_core::ops::PeerView { code: "PM8Tpeer".into(), contact: None, receive_addresses: 1, send_addresses: 0 }];
    // `]` moves from Contacts to Channels within People.
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Channels);
    press(&mut app, KeyCode::Char('a'));
    match &app.modal {
        Modal::Form(f) => {
            assert_eq!(f.kind, FormKind::Contact(None));
            assert_eq!(f.fields[2].value, "PM8Tpeer");
        }
        _ => panic!("expected the add-contact form prefilled with the channel's code"),
    }
}

// ------------------------------------------------------ routing, MAX and pools

/// The live Cyprus-1 shape in miniature: a WQI cluster, a WQUAI cluster, and the WQI/WQUAI
/// pool that bridges them.
fn pool_shape() -> Vec<wallet_core::markets::Pool> {
    use wallet_core::markets::{Pool, PoolToken};
    let tok = |a: &str, s: &str| PoolToken { address: a.into(), symbol: s.into(), decimals: 18 };
    let pool = |a: (&str, &str), b: (&str, &str), tvl: f64| Pool {
        address: format!("0x00pair{}{}", a.1, b.1),
        token0: tok(a.0, a.1),
        token1: tok(b.0, b.1),
        reserve0: 100.0,
        reserve1: 100.0,
        tvl_usd: Some(tvl),
        volume_24h_usd: None,
        ..Default::default()
    };
    let wquai = wallet_core::sdk::wrappers::WQUAI_MAINNET_ADDRESS;
    let wqi = wallet_core::sdk::wrappers::WQI_ADDRESS;
    vec![
        pool((wqi, "WQI"), (wquai, "WQUAI"), 67_150.0),
        pool(("0x00a1", "SMOL"), (wqi, "WQI"), 1_157.0),
        pool(("0x00b1", "LAPTOP"), (wquai, "WQUAI"), 510.0),
        pool(("0x00b2", "NVNT"), (wquai, "WQUAI"), 4.25),
    ]
}

fn with_pools(app: &mut App) {
    app.network_id = "mainnet".into();
    app.eco.markets_view.pools = Some(Ok((pool_shape(), wallet_core::markets::DexOverview::default())));
}

/// A pair that needs two hubs is offered, and one with no pool at all is refused rather than
/// let through to fail as "no Quainance pool route".
#[test]
fn the_token_picker_only_offers_pairs_that_have_a_route() {
    use super::super::eco::RouteState;
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    // Paying SMOL, choosing what to receive.
    app.eco.swap.from = SwapAsset::Token { address: "0x00a1".into(), symbol: "SMOL".into(), decimals: 18 };
    let entries = app.picker_entries("", false);
    let route_of = |entries: &[super::super::eco::PickerEntry], symbol: &str| {
        entries.iter().find(|e| e.asset.symbol() == symbol).map(|e| e.route.clone())
    };
    // LAPTOP sits on the other side of the DEX: reachable only through WQI → WQUAI.
    match route_of(&entries, "LAPTOP") {
        Some(RouteState::Fillable(info)) => {
            assert_eq!(info.hops(), 3, "{}", info.text());
            assert_eq!(info.fee_bps(), 90);
        }
        other => panic!("LAPTOP should be reachable in three hops, got {other:?}"),
    }
    // A token with no pool anywhere cannot be chosen.
    app.eco.markets = vec![];
    app.eco.portfolio = Some(wallet_core::portfolio::Portfolio {
        rows: vec![asset_row(wallet_core::portfolio::AssetKey::Token("0x00dead".into()), "GHOST", "0", true)],
        ..Default::default()
    });
    let entries = app.picker_entries("", false);
    assert_eq!(route_of(&entries, "GHOST"), Some(RouteState::Dead));
    assert!(!RouteState::Dead.choosable());
    // Dead rows sort below fillable ones.
    let dead_at = entries.iter().position(|e| e.asset.symbol() == "GHOST").unwrap();
    let live_at = entries.iter().position(|e| e.asset.symbol() == "LAPTOP").unwrap();
    assert!(live_at < dead_at, "a reachable token outranks an unreachable one");
}

/// Before pools load, nothing is filtered — the picker never refuses on ignorance.
#[test]
fn the_picker_filters_nothing_until_pools_load() {
    use super::super::eco::RouteState;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.network_id = "mainnet".into();
    assert!(app.route_graph().is_empty());
    assert!(app.picker_entries("", false).iter().all(|e| e.route == RouteState::Unknown));
    assert!(RouteState::Unknown.choosable());
}

/// MAX on native QUAI keeps a fee back; MAX on a token takes the lot.
#[test]
fn max_reserves_gas_for_quai_and_not_for_tokens() {
    use wallet_core::sdk::U256;
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let quai = U256::from(100u64) * U256::from(10u128.pow(18));
    app.eco.portfolio = Some(wallet_core::portfolio::Portfolio {
        rows: vec![
            asset_row(wallet_core::portfolio::AssetKey::Quai, "QUAI", &quai.to_string(), true),
            asset_row(wallet_core::portfolio::AssetKey::Token("0x00a1".into()), "SMOL", "12345", true),
        ],
        ..Default::default()
    });
    app.switch(Screen::Swap);
    // Without a gas price the wallet says so rather than guessing a reserve.
    app.eco.swap.from = SwapAsset::Quai;
    sheet(&mut app, 'm');
    assert!(app.eco.swap.amount.is_empty());
    assert!(app.toasts.iter().any(|t| t.text.contains("gas price")), "{:?}", app.toasts);
    // With one, MAX fills the balance less the worst-case fee.
    app.eco.gas_price = Some(U256::from(23_800_691_942_794u64));
    sheet(&mut app, 'm');
    let filled: f64 = app.eco.swap.amount.parse().unwrap();
    assert!(filled > 78.0 && filled < 80.0, "MAX filled {filled} QUAI of 100");
    assert!(app.toasts.iter().any(|t| t.text.starts_with("MAX leaves")), "{:?}", app.toasts);
    // A token has no fee to hold back.
    app.eco.swap.from = SwapAsset::Token { address: "0x00a1".into(), symbol: "SMOL".into(), decimals: 18 };
    sheet(&mut app, 'm');
    assert_eq!(app.eco.swap.amount, "0.000000000000012345");
}

/// `%` steps through a quarter, half, three quarters and all of MAX; typing clears the share.
#[test]
fn percent_steps_through_shares_of_max() {
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let whole = (400u128 * 10u128.pow(18)).to_string();
    app.eco.portfolio = Some(wallet_core::portfolio::Portfolio {
        rows: vec![asset_row(wallet_core::portfolio::AssetKey::Token("0x00a1".into()), "SMOL", &whole, true)],
        ..Default::default()
    });
    app.switch(Screen::Swap);
    app.eco.swap.from = SwapAsset::Token { address: "0x00a1".into(), symbol: "SMOL".into(), decimals: 18 };
    for (want, share) in [("100", 25), ("200", 50), ("300", 75), ("400", 100), ("100", 25)] {
        sheet(&mut app, 'p');
        assert_eq!(app.eco.swap.amount, want);
        assert_eq!(app.eco.swap.preset, Some(share));
    }
    press(&mut app, KeyCode::Char('5'));
    assert_eq!(app.eco.swap.preset, None, "a typed amount is not a share any more");
}

/// An indexer's rounded balance is refused: filling MAX from it would build a reverting swap.
#[test]
fn max_refuses_an_inexact_balance() {
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.eco.portfolio = Some(wallet_core::portfolio::Portfolio {
        rows: vec![asset_row(wallet_core::portfolio::AssetKey::Token("0x00a1".into()), "SMOL", "999", false)],
        ..Default::default()
    });
    app.switch(Screen::Swap);
    app.eco.swap.from = SwapAsset::Token { address: "0x00a1".into(), symbol: "SMOL".into(), decimals: 18 };
    sheet(&mut app, 'm');
    assert!(app.eco.swap.amount.is_empty(), "nothing is filled from a rounded balance");
    assert!(app.toasts.iter().any(|t| t.text.contains("loading")), "{:?}", app.toasts);
}

/// MAX waits for the actual fee quote and rejects replies after an amount, direction or network edit.
#[test]
fn qi_max_uses_a_correlated_fee_quote_and_preserves_edits() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Convert);
    app.eco.convert.qi_to_quai = true;
    let result = || wallet_core::ops::QiSpecialMax {
        amount: "8".into(),
        amount_qits: "8000".into(),
        fee_qits: "27".into(),
        inputs: 2,
        outputs: 3,
        excluded_inputs: 0,
        excluded_qits: "0".into(),
        candidate_height: "100".into(),
        whole_qi: true,
    };
    sheet(&mut app, 'm');
    assert!(app.eco.convert.amount.is_empty(), "a balance heuristic must not populate MAX");
    let first = app.eco.max_request.unwrap().0;
    app.eco.convert.amount = "4".into();
    app.on_event(Ev::QiMax { key: first, result: Ok(result()) }, (100, 30));
    assert_eq!(app.eco.convert.amount, "4");
    sheet(&mut app, 'm');
    let second = app.eco.max_request.unwrap().0;
    app.on_event(Ev::QiMax { key: first, result: Ok(result()) }, (100, 30));
    assert_eq!(app.eco.convert.amount, "4", "a superseded identical quote cannot fill the field");
    app.on_event(Ev::QiMax { key: second, result: Ok(result()) }, (100, 30));
    assert_eq!(app.eco.convert.amount, "8");
    assert!(app.eco.convert.quote.is_none());
    assert!(app.toasts.iter().any(|t| t.text.contains("27 qits")));
}

/// The indexer prices token1 per token0. When the wallet shows the pair the other way round
/// the chart must invert — including swapping which extreme is the high.
#[test]
fn indexer_candles_are_inverted_when_the_pair_is_shown_the_other_way() {
    use wallet_core::markets::{Candle, Pool, PoolToken};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let tok = |a: &str, s: &str| PoolToken { address: a.into(), symbol: s.into(), decimals: 18 };
    let pool = Pool {
        address: "0x00pair".into(),
        token0: tok("0x00a", "WQI"),
        token1: tok("0x00b", "WQUAI"),
        reserve0: 1.0,
        reserve1: 100.0,
        tvl_usd: None,
        volume_24h_usd: None,
        ..Default::default()
    };
    let candle = Candle { start: 100, open: 100.0, high: 200.0, low: 50.0, close: 125.0, volume: 7.0, trades: 3 };
    app.eco.markets_view.candles.insert(("0x00pair".into(), 3_600), vec![candle.clone()]);
    // base0 = true: shown as the indexer prices it.
    let same = app.chart_candles(&pool, true, 3_600, 10);
    assert_eq!(same.as_ref(), &vec![candle.clone()]);
    // base0 = false: every price inverts and high/low swap ends.
    let flipped = app.chart_candles(&pool, false, 3_600, 10);
    let f = &flipped[0];
    assert!((f.open - 0.01).abs() < 1e-12, "{f:?}");
    assert!((f.close - 1.0 / 125.0).abs() < 1e-12, "{f:?}");
    assert!((f.high - 1.0 / 50.0).abs() < 1e-12, "the old low is the new high: {f:?}");
    assert!((f.low - 1.0 / 200.0).abs() < 1e-12, "the old high is the new low: {f:?}");
    assert!(f.low <= f.open && f.open <= f.high, "still a well-formed candle");
    assert_eq!((f.volume, f.trades), (7.0, 3), "volume and count do not invert");
    // With no indexer candles it falls back to the log-built ones (none here, so empty).
    app.eco.markets_view.candles.clear();
    assert!(app.chart_candles(&pool, true, 3_600, 10).is_empty());
}

#[test]
fn market_derived_cache_reuses_frames_and_invalidates_changed_history_and_windows() {
    use std::sync::Arc;
    use wallet_core::markets::{Candle, Pool, PoolEvent, PoolToken};
    use wallet_core::sdk::U256;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let pool = Pool {
        address: "memo-pair".into(),
        token0: PoolToken { decimals: 0, ..PoolToken::default() },
        token1: PoolToken { decimals: 0, ..PoolToken::default() },
        ..Pool::default()
    };
    let event = |reserve| PoolEvent::Sync {
        at: 3590,
        block: 1,
        index: 0,
        tx: "tx".into(),
        reserve0: U256::from(10),
        reserve1: U256::from(reserve),
    };
    let data = |events| super::super::data::DataEv::PoolEvents { pool: pool.address.clone(), coverage: None, result: Ok(events) };
    app.on_data_event(data(vec![event(20)]));
    let first = app.chart_candles_at(&pool, true, 60, 3, 3601);
    let repeat = app.chart_candles_at(&pool, true, 60, 3, 3610);
    assert!(Arc::ptr_eq(&first, &repeat), "unchanged frames reuse derived candles");
    app.on_data_event(data(vec![event(20)]));
    assert!(Arc::ptr_eq(&first, &app.chart_candles_at(&pool, true, 60, 3, 3610)), "identical refresh keeps its revision");
    app.on_data_event(data(vec![event(30)]));
    let repaired = app.chart_candles_at(&pool, true, 60, 3, 3610);
    assert!(!Arc::ptr_eq(&first, &repaired), "same-position repair changes the revision");
    assert_eq!(repaired.last().unwrap().close, 3.0);
    let flipped = app.chart_candles_at(&pool, false, 60, 3, 3610);
    assert!((flipped.last().unwrap().close - 1.0 / 3.0).abs() < 1e-12);
    let boundary = app.chart_candles_at(&pool, true, 60, 3, 3660);
    assert_eq!(boundary.last().unwrap().start, 3660);
    assert!(!Arc::ptr_eq(&boundary, &repaired));
    let narrower = app.chart_candles_at(&pool, true, 60, 1, 3610);
    assert_eq!(narrower.len(), 1);
    let mut decimals = pool.clone();
    decimals.token1.decimals = 1;
    assert!((app.chart_candles_at(&decimals, true, 60, 3, 3610).last().unwrap().close - 0.3).abs() < 1e-12);
    app.on_data_event(super::super::data::DataEv::PairCandles {
        pool: pool.address.clone(),
        bucket: 60,
        candles: vec![Candle { start: 3540, open: 7.0, high: 7.0, low: 7.0, close: 7.0, volume: 5.0, trades: 1 }],
    });
    let indexed = app.chart_candles_at(&pool, true, 60, 3, 3610);
    assert!(!Arc::ptr_eq(&indexed, &repaired), "new indexed history invalidates the merged chart");
    assert_eq!(indexed.first().unwrap().open, 7.0);
    let tape = app.market_trades(&pool, true);
    assert!(Arc::ptr_eq(&tape, &app.market_trades(&pool, true)));
    for count in 1..40 {
        app.chart_candles_at(&pool, true, 60, count, 3610);
    }
    assert!(!Arc::ptr_eq(&indexed, &app.chart_candles_at(&pool, true, 60, 3, 3610)), "bounded cache evicts old view combinations");
    let mut empty = Pool { address: "fallback".into(), reserve0: 2.0, reserve1: 10.0, ..Pool::default() };
    assert_eq!(app.market_stats(&empty, true, 3610).price, Some(5.0));
    empty.reserve1 = 20.0;
    assert_eq!(app.market_stats(&empty, true, 3610).price, Some(10.0), "live reserve fallback changes the cache key");
}

/// A sealed conversation needs only two payment codes. A contact who has one must therefore
/// appear on the board — otherwise a message that has already arrived stays invisible.
#[test]
fn a_contact_with_a_payment_code_can_be_opened_without_a_payment_channel() {
    use super::super::eco::BoardRow;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let contact = |name: &str, code: Option<&str>, address: Option<&str>| wallet_core::appdb::Contact {
        id: 1,
        name: name.into(),
        address: address.map(str::to_string),
        payment_code: code.map(str::to_string),
        note: String::new(),
    };
    app.dash.contacts = vec![
        contact("alice", Some("PM8Talice"), Some("0x00a1")),
        // No payment code: cannot hold a sealed conversation, so no row.
        contact("bob", None, Some("0x00b1")),
    ];
    assert!(app.dash.peers.is_empty(), "no payment channel has been established");
    let rows = app.board_rows();
    assert!(
        rows.iter().any(|r| matches!(r, BoardRow::Peer(code, name) if code == "PM8Talice" && name.as_deref() == Some("alice"))),
        "a contact with a payment code is reachable: {rows:?}"
    );
    assert!(!rows.iter().any(|r| matches!(r, BoardRow::Peer(_, name) if name.as_deref() == Some("bob"))));
    // An established channel and a contact for the same code produce one row, not two.
    app.dash.peers = vec![wallet_core::ops::PeerView {
        code: "PM8Talice".into(),
        contact: Some("alice".into()),
        receive_addresses: 1,
        send_addresses: 0,
    }];
    let peers = app.board_rows().into_iter().filter(|r| matches!(r, BoardRow::Peer(..))).count();
    assert_eq!(peers, 1, "the channel and the contact are the same person");
}

/// Board posts from someone in your contacts read as their name.
#[test]
fn board_posts_from_contacts_show_the_contact_name() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.contacts = vec![wallet_core::appdb::Contact {
        id: 1,
        name: "alice".into(),
        address: Some("0x00A1B2C3D4E5F600000000000000000000000001".into()),
        payment_code: None,
        note: String::new(),
    }];
    // Case-insensitive, because an address from a log is lowercase.
    assert_eq!(app.contact_name_for("0x00a1b2c3d4e5f600000000000000000000000001").as_deref(), Some("alice"));
    assert_eq!(app.contact_name_for("0x00ffffffffffffffffffffffffffffffffffffff"), None, "a stranger stays an address");
}

/// Scrolling the markets list is not a request for every row it passes over.
///
/// Each row the cursor lands on would otherwise fetch that pool's logs and candles, so holding a
/// cursor key down the list cost one round of requests per row — most of them for pools the user
/// never stopped at. A row is only asked about once the cursor has rested on it.
#[test]
fn scrolling_markets_only_fetches_the_row_the_cursor_settles_on() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    // A directory that was just loaded, so the tick goes on to the selected pool.
    app.eco.markets_view.pools_at = Some(Instant::now());
    app.switch(Screen::Markets);
    let asked = |app: &App| -> Vec<String> {
        let mut rows: Vec<String> = app.eco.markets_view.events_at.keys().cloned().collect();
        rows.sort();
        rows
    };
    // Scroll through every row faster than the cursor can settle on any of them.
    for row in 0..app.eco.markets_view.pools.as_ref().unwrap().as_ref().unwrap().0.len() {
        app.selected = row;
        app.tick_markets();
    }
    assert!(asked(&app).is_empty(), "a row scrolled past is not a row asked about: {:?}", asked(&app));
    // Stop on the last one. Once it has been there long enough, it is fetched — once.
    std::thread::sleep(crate::tui::eco::SELECTION_SETTLES + std::time::Duration::from_millis(50));
    app.tick_markets();
    let settled = app.selected_pool().expect("a pool under the cursor").address;
    assert_eq!(asked(&app), vec![settled.clone()], "only the row the cursor stopped on");
    app.tick_markets();
    assert_eq!(asked(&app), vec![settled], "and asking again while it is still fresh changes nothing");
}

/// Pools is a Markets tab, and its actions refuse clearly when there is nothing to act on.
#[test]
fn pools_tab_navigates_and_guards_its_actions() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Markets);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Launches);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Pools, "Pools sits beside Launches in Markets");
    assert_eq!(app.breadcrumb(), vec!["Markets".to_string(), "Pools".to_string()]);
    // With no positions, acting on the (empty) positions pane says so rather than doing nothing.
    app.eco.pools_view.positions = Some(Ok(vec![]));
    sheet(&mut app, 's');
    assert!(app.toasts.iter().any(|t| t.text.contains("no pool selected")), "{:?}", app.toasts);
}

/// A new position starts from the pool directory, not from something already held — holding
/// nothing must not mean being unable to add liquidity.
#[test]
fn liquidity_can_be_added_to_a_pool_with_no_position() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.switch(Screen::Pools);
    app.dash.unlocked = true;
    app.eco.pools_view.positions = Some(Ok(vec![]));
    // Tab moves to the directory, which lists every pool whether or not we are in it.
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.pane, 1);
    assert!(!app.directory_rows().is_empty(), "the directory is the pool list");
    // Pick the SMOL pool and add liquidity to it.
    let smol = app.directory_rows().iter().position(|p| p.token0.symbol == "SMOL" || p.token1.symbol == "SMOL").unwrap();
    app.eco.pools_view.pool_selected = smol;
    let focus = app.focused_pool().unwrap();
    assert!(focus.name.contains("SMOL"), "{}", focus.name);
    assert!(focus.position.is_none(), "nothing held in it yet");
    press(&mut app, KeyCode::Char('a'));
    let card = app.eco.pools_view.add.as_ref().expect("the deposit card for a pool we hold nothing in");
    assert_eq!(card.pair, focus.pair);
    assert_eq!(
        (card.token0.symbol.as_str(), card.token1.symbol.as_str()),
        (focus.tokens.0.symbol.as_str(), focus.tokens.1.symbol.as_str())
    );
    press(&mut app, KeyCode::Esc);
    assert!(app.eco.pools_view.add.is_none(), "esc abandons the deposit");
    // Actions that need a position still refuse, and name the pool so the message is useful.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.toasts.iter().any(|t| t.text.contains("no liquidity in") && t.text.contains("press a")), "{:?}", app.toasts);
}

/// Staking is offered only where a gauge exists and there is unstaked LP to stake.
#[test]
fn pool_actions_follow_the_position() {
    use wallet_core::liquidity::LpPosition;
    use wallet_core::markets::PoolToken;
    use wallet_core::sdk::U256;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Pools);
    app.dash.unlocked = true;
    let tok = |s: &str| PoolToken { address: format!("0x00{s}"), symbol: s.into(), decimals: 18 };
    let e18 = |n: u128| U256::from(n) * U256::from(10u128.pow(18));
    let base =
        LpPosition { pair: "0x00pair".into(), token0: tok("WQI"), token1: tok("WQUAI"), lp_total: e18(1_000), ..LpPosition::default() };
    // No gauge on this pair: staking is refused with the reason.
    app.eco.pools_view.positions = Some(Ok(vec![LpPosition { lp_wallet: e18(10), pid: None, ..base.clone() }]));
    sheet(&mut app, 's');
    assert!(app.toasts.iter().any(|t| t.text.contains("in no gauge")), "{:?}", app.toasts);
    app.toasts.clear();
    // A launch-zone gauge: staking starts the approve-then-stake walk instead of refusing, and
    // funding is refused because those campaigns are funded at launch.
    let zone = LpPosition { lp_wallet: e18(10), pid: Some(0), gauge: Some(wallet_core::gauge::GaugeKind::Zone), ..base.clone() };
    app.eco.pools_view.positions = Some(Ok(vec![zone]));
    sheet(&mut app, 'i');
    assert!(app.toasts.iter().any(|t| t.text.contains("funded when a token launches")), "{:?}", app.toasts);
    sheet(&mut app, 's');
    assert!(app.toasts.iter().all(|t| !t.text.contains("gauge —")), "{:?}", app.toasts);
    // Staking opens its form first, so the gauge and the amount are chosen before anything is
    // prepared; the walk itself starts when that form is submitted.
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(f.kind, FormKind::StakePosition { stake: true, .. })), "the stake form opened");
    app.modal = Modal::None;
    app.eco.flow = None;
    app.toasts.clear();
    // A gauge but nothing staked: unstaking is refused.
    app.eco.pools_view.positions = Some(Ok(vec![LpPosition { lp_wallet: e18(10), pid: Some(0), ..base.clone() }]));
    sheet(&mut app, 'u');
    assert!(app.toasts.iter().any(|t| t.text.contains("nothing staked")), "{:?}", app.toasts);
    app.toasts.clear();
    // Adding opens a card carrying the pair, so the review cannot target the wrong pool.
    press(&mut app, KeyCode::Char('a'));
    let card = app.eco.pools_view.add.as_ref().expect("the deposit card");
    assert_eq!(card.pair, "0x00pair");
    assert_eq!(card.name, "WQI/WQUAI");
}

/// h, s and e are also the app's left, send and edit; on Pools they must still act on the
/// position straight from the list, not only from the action sheet.
#[test]
fn pool_keys_beat_the_app_verbs_they_share() {
    use wallet_core::liquidity::LpPosition;
    use wallet_core::markets::PoolToken;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let (worker, prepared) = Worker::capture_prepares();
    app.worker = Some(worker);
    app.switch(Screen::Pools);
    app.dash.unlocked = true;
    let tok = |s: &str| PoolToken { address: format!("0x00{s}"), symbol: s.into(), decimals: 18 };
    let e18 = |n: u128| U256::from(n) * U256::from(10u128.pow(18));
    app.eco.pools_view.positions = Some(Ok(vec![LpPosition {
        pair: "0x00pair".into(),
        token0: tok("WQI"),
        token1: tok("WQUAI"),
        lp_total: e18(1_000),
        lp_wallet: e18(10),
        lp_staked: e18(5),
        pid: Some(0),
        ..LpPosition::default()
    }]));
    let wait = std::time::Duration::from_secs(5);
    press(&mut app, KeyCode::Char('h'));
    assert!(matches!(prepared.recv_timeout(wait), Ok(Prepare::Harvest { exit: false, ref pair, .. }) if pair == "0x00pair"), "h harvests");
    press(&mut app, KeyCode::Char('e'));
    assert!(matches!(prepared.recv_timeout(wait), Ok(Prepare::Harvest { exit: true, .. })), "e exits");
    press(&mut app, KeyCode::Char('s'));
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(f.kind, FormKind::StakePosition { stake: true, .. })), "s stakes");
    app.modal = Modal::None;
    press(&mut app, KeyCode::Char('u'));
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(f.kind, FormKind::StakePosition { stake: false, .. })), "u unstakes");
    app.modal = Modal::None;
    press(&mut app, KeyCode::Char('r'));
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(f.kind, FormKind::RemoveLiquidity { .. })), "r removes, not receive");
    app.modal = Modal::None;
    // The left arrow is still only left: it must never be a way to harvest.
    press(&mut app, KeyCode::Left);
    assert!(prepared.recv_timeout(std::time::Duration::from_millis(200)).is_err(), "left prepared something");
    // With the movement letters off, h is still the pool's own key.
    app.config.vim_keys = false;
    press(&mut app, KeyCode::Char('h'));
    assert!(matches!(prepared.recv_timeout(wait), Ok(Prepare::Harvest { exit: false, .. })), "h harvests with vim keys off");
}

/// Off, h j k l move nothing; the arrows always do.
#[test]
fn movement_letters_can_be_turned_off() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Settings);
    app.selected = 0;
    app.config.vim_keys = false;
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected, 0, "j is not down");
    press(&mut app, KeyCode::Down);
    assert_eq!(app.selected, 1, "the arrow is");
    app.config.vim_keys = true;
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.selected, 2, "on again, j is down");
}

/// The token you have least of is what limits a deposit, so the form lets either side be the
/// one that is typed — and the field order is what `submit_form` reads by index.
#[test]
fn a_deposit_can_be_typed_in_either_side_of_the_pool() {
    use wallet_core::liquidity::LpPosition;
    use wallet_core::markets::PoolToken;
    use wallet_core::sdk::U256;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Pools);
    app.dash.unlocked = true;
    let tok = |s: &str| PoolToken { address: format!("0x00{s}"), symbol: s.into(), decimals: 18 };
    let e18 = |n: u128| U256::from(n) * U256::from(10u128.pow(18));
    app.eco.pools_view.positions = Some(Ok(vec![LpPosition {
        pair: "0x00pair".into(),
        token0: tok("SMOL"),
        token1: tok("WQI"),
        lp_wallet: e18(10),
        lp_total: e18(1_000),
        ..LpPosition::default()
    }]));
    press(&mut app, KeyCode::Char('a'));
    // The card opens on the first token's row, and typing there sizes the deposit by SMOL.
    for c in "12".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    let card = app.eco.pools_view.add.as_ref().expect("the deposit card");
    assert_eq!((card.field, card.side1, card.amount.as_str()), (1, false, "12"));
    assert_eq!(card.typed().symbol, "SMOL");
    // Typing in the other row moves the deposit to that side and starts its amount fresh,
    // because the two are never both typed.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char('3'));
    let card = app.eco.pools_view.add.as_ref().expect("the deposit card stays open");
    assert_eq!((card.field, card.side1, card.amount.as_str()), (2, true, "3"));
    assert_eq!(card.typed().symbol, "WQI", "the WQI row is the one being typed");
    assert_eq!(card.paired().symbol, "SMOL", "and SMOL is what the pool will ask for");
}

/// An HD wallet with a real vault (production key derivation), locked on the lock screen.
fn locked_hd_app() -> (tempfile::TempDir, App) {
    let dir = tempfile::tempdir().unwrap();
    let paths = wallet_core::paths::Paths::resolve(Some(dir.path().to_path_buf())).unwrap();
    let registry = wallet_core::registry::Registry::new(paths.clone());
    let phrase = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";
    let meta = registry.create_hd("vaulted", phrase, "english", "", "password123", true).unwrap();
    let caps = super::super::terminal::detect(wallet_core::config::GraphicsMode::Cells);
    let mut app = App::new(paths, "local".into(), AppConfig::default(), Theme::terminal(false), caps, Some(meta));
    app.onboarding = None;
    app.locked = true;
    app.modal = Modal::None;
    (dir, app)
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

/// Wait for the password check running on its own thread.
fn await_unlock(app: &mut App) {
    let started = Instant::now();
    while app.unlock_check.is_some() {
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "the password check never answered");
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.poll_unlock();
    }
}

/// The lock screen checks the password itself, so an unlock never waits for the worker: it says
/// it is unlocking at once, keeps a refusal readable, and opens without the worker's say-so.
#[test]
fn the_lock_screen_unlocks_without_the_worker() {
    let (_dir, mut app) = locked_hd_app();
    let size = (100, 30);
    type_text(&mut app, "wrongpassword");
    press(&mut app, KeyCode::Enter);
    // Submitted: said immediately, and the password is gone from the screen's own copy.
    assert!(app.unlocking, "the screen says it is unlocking");
    assert!(app.unlock_check.is_some(), "checked off the render thread");
    assert!(app.lock_input.is_empty());
    // Keys are ignored until it answers, so they cannot land in the next attempt.
    press(&mut app, KeyCode::Char('x'));
    assert!(app.lock_input.is_empty(), "typing during an unlock is not collected");
    // Background work failing meanwhile is not the answer, and does not end the attempt.
    app.on_event(Ev::Error("node did not answer within 12s".into()), size);
    assert!(app.unlocking && app.lock_error.is_none());
    await_unlock(&mut app);
    // Refused, and said plainly: the vault's own wording leads with the damaged-file case.
    assert!(!app.unlocking && app.locked);
    assert_eq!(app.lock_error.as_deref(), Some("That password didn't open this wallet. Caps Lock? (A damaged vault file looks the same.)"));
    // Background work reports busy, which must not take the error's place.
    app.on_event(Ev::Busy(Some("syncing…".into())), size);
    assert_eq!(app.lock_error.as_deref(), Some("That password didn't open this wallet. Caps Lock? (A damaged vault file looks the same.)"));
    // Typing again clears it; the right password opens the wallet with no worker running at all.
    type_text(&mut app, "password123");
    assert!(app.lock_error.is_none());
    press(&mut app, KeyCode::Enter);
    await_unlock(&mut app);
    assert!(!app.locked && !app.unlocking && app.lock_error.is_none());
}

/// A switch locks the screen at once. The worker confirms it later, possibly after the new wallet
/// was already unlocked, and that late confirmation must not lock it again; a switch that failed
/// in the worker takes the screen back to locked.
#[test]
fn a_switch_locks_at_once_and_its_late_confirmation_does_not_relock() {
    let (_dir, mut app) = locked_hd_app();
    let size = (100, 30);
    app.locked = false;
    let other = app
        .registry
        .create_hd(
            "second",
            "legal winner thank year wave sausage worth useful legal winner thank yellow",
            "english",
            "",
            "password123",
            true,
        )
        .unwrap();
    app.switch_wallet(&other.id);
    assert!(app.locked, "the new wallet's lock screen shows before the worker gets there");
    type_text(&mut app, "password123");
    press(&mut app, KeyCode::Enter);
    await_unlock(&mut app);
    assert!(!app.locked);
    app.on_event(Ev::Locked, size);
    assert!(!app.locked, "the switch's own confirmation is stale by now");
    // A lock after that is a real one.
    app.on_event(Ev::Locked, size);
    assert!(app.locked);
    // The keys reached a worker whose switch failed: lock again and say so.
    app.locked = false;
    app.on_event(Ev::KeysRefused, size);
    assert!(app.locked && app.lock_error.is_some());
}

#[test]
fn sections_tabs_and_detail_stack() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    assert_eq!(app.screen, Screen::Home);
    // Qi coins and accounts (with the time locks under them) are Home's sub-tabs.
    assert_eq!(app.breadcrumb(), vec!["Home".to_string(), "Portfolio".to_string()]);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Qi);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Accounts);
    assert_eq!(app.breadcrumb(), vec!["Home".to_string(), "Accounts".to_string()]);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Home);
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.screen, Screen::Accounts);
    // The section remembers its tab.
    press(&mut app, KeyCode::Char('6'));
    assert_eq!(app.screen, Screen::Activity);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.activity_filter, ActivityFilter::Sends);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.screen, Screen::Accounts);
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.screen.section(), Section::Markets);
    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.screen.section(), Section::Trade);
    // Exchange, Orders and PnL: Swap, Convert and Wrap share the Exchange tab, which returns to
    // whichever of them was last open.
    assert_eq!(app.screen, Screen::Swap);
    assert_eq!(Section::Trade.tab_labels(&app.config.features), vec!["Exchange", "Orders", "PnL"]);
    app.switch(Screen::Wrap);
    assert_eq!(app.breadcrumb(), vec!["Trade".to_string(), "Exchange".to_string()]);
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(app.screen, Screen::Orders);
    press(&mut app, KeyCode::Char('['));
    assert_eq!(app.screen, Screen::Wrap, "the Exchange tab returns to the view last open");
    // Tab moves pane focus, it no longer switches screens.
    app.switch(Screen::Home);
    press(&mut app, KeyCode::Tab);
    assert_eq!((app.screen, app.pane), (Screen::Home, 1));
    // Detail stack: Enter pushes, Esc pops.
    app.eco.portfolio = Some(wallet_core::portfolio::Portfolio {
        rows: vec![wallet_core::portfolio::AssetRow {
            key: wallet_core::portfolio::AssetKey::Quai,
            symbol: "QUAI".into(),
            name: "Quai".into(),
            balance: "1".into(),
            decimals: 18,
            exact: true,
            price_usd: None,
            price_kind: wallet_core::portfolio::PriceKind::None,
            price_source: String::new(),
            price_at: 0,
            value_usd: None,
            allocation: 0.0,
            change_24h: None,
            icon_url: None,
            trust: wallet_core::portfolio::Trust::Verified,
            holders: None,
        }],
        ..Default::default()
    });
    app.switch(Screen::Home);
    app.pane = 0;
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.detail, vec![Detail::Asset("quai".into())]);
    assert_eq!(app.breadcrumb().last().map(String::as_str), Some("QUAI"));
    press(&mut app, KeyCode::Esc);
    assert!(app.detail.is_empty());
    // `t` opens Swap with the focused token as the pay side and the amount focused.
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.screen, Screen::Swap);
    assert_eq!(app.eco.swap.from, wallet_core::swap::SwapAsset::Quai);
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Char('.'));
    press(&mut app, KeyCode::Char('5'));
    assert_eq!(app.eco.swap.amount, "1.5");
    assert_eq!(app.screen, Screen::Swap);
    // Esc unfocuses the input; number keys navigate again.
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('6'));
    assert_eq!(app.screen, Screen::Activity);
    // Arriving by section key leaves the card unfocused.
    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.screen, Screen::Swap);
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.screen, Screen::Home);
}

/// On Launches, a token on its curve is bought and sold from the curve: `b` and `t` open the buy
/// form, `S` opens the sale prefilled with what the wallet holds, and a token that has left its
/// curve refuses both and says why.
#[test]
fn launches_trade_on_the_curve_while_a_token_is_bonding() {
    use wallet_core::launches::{Launch, Phase};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Launches);
    app.dash.unlocked = true;
    let launch = |symbol: &str, phase| Launch {
        token: format!("0x00{:0>38}", symbol.len()),
        symbol: symbol.into(),
        phase,
        curve: Some("0x004ce1cbb33cad511b79d52c6e1118ce4eb60db3".into()),
        ..Default::default()
    };
    app.eco.launches = Some(Ok(vec![launch("CHEEZ", Phase::Bonding), launch("QOGE", Phase::Graduated)]));
    let e18 = |n: u128| wallet_core::sdk::U256::from(n) * wallet_core::sdk::U256::from(10u128.pow(18));
    let token = app.launch_rows()[0].token.clone();
    app.eco.curves.insert(
        token.clone(),
        Ok(wallet_core::curve::CurveMarket { token_decimals: 18, token: token.clone(), held: e18(1_250), ..Default::default() }),
    );
    app.selected = 0;
    press(&mut app, KeyCode::Char('b'));
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(&f.kind, FormKind::CurveBuy { symbol, .. } if symbol == "CHEEZ")));
    app.modal = Modal::None;
    press(&mut app, KeyCode::Char('S'));
    match &app.modal {
        Modal::Form(f) => {
            assert!(matches!(&f.kind, FormKind::CurveSell { .. }));
            assert_eq!(f.fields[1].value, "1250", "prefilled with what is held");
        }
        _ => panic!("expected the sell form"),
    }
    app.modal = Modal::None;
    press(&mut app, KeyCode::Char('t'));
    assert!(matches!(&app.modal, Modal::Form(f) if matches!(f.kind, FormKind::CurveBuy { .. })), "t buys on the curve too");
    app.modal = Modal::None;
    app.selected = 1;
    press(&mut app, KeyCode::Char('b'));
    assert!(matches!(app.modal, Modal::None), "no form for a graduated token");
    assert!(app.toasts.iter().any(|t| t.text.contains("left its curve")), "{:?}", app.toasts);
}

/// A route across both exchanges runs as two swaps: the first ends on the hub, and only once it
/// confirms — and its receipt says what it paid — is the second review asked for, for exactly
/// that amount and on to where the route ends.
#[test]
fn a_two_exchange_route_swaps_to_the_hub_then_sizes_the_second_swap_from_the_first() {
    use super::super::eco::FlowKind;
    use wallet_core::appdb::OpStatus;
    use wallet_core::markets::Venue;
    use wallet_core::swap::{SwapAsset, SwapLeg, SwapQuote};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.network_id = "mainnet".into();
    app.dash.unlocked = true;
    let size = (100, 30);
    let wquai = app.config.network("mainnet").unwrap().wquai.unwrap().to_lowercase();
    let usdt = "0x0049f7cbca3556c2dfae62aafa7015f99de1b8f5".to_string();
    let qoge = SwapAsset::Token { address: "0x0048848ca70ea1560577b4725a84b23b6bc589e2".into(), symbol: "QOGE".into(), decimals: 18 };
    let usdt_asset = SwapAsset::Token { address: usdt.clone(), symbol: "USDT".into(), decimals: 6 };
    // A trading intent names the signer before it carries any amount, so the route needs an
    // account selected before it will start at all.
    let qoge_address = "0x0048848ca70ea1560577b4725a84b23b6bc589e2".to_string();
    let owner = "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".to_string();
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: owner.clone(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::from(100u64) * U256::from(10u64).pow(U256::from(18)),
        locked: U256::ZERO,
        nonce: 0,
    }];
    let leg = |venue, path: Vec<String>, route: &[&str], decimals| SwapLeg {
        venue,
        router: "0x00".into(),
        path,
        route: route.iter().map(|s| s.to_string()).collect(),
        pools: vec![],
        amount_in: "1".into(),
        amount_out: "1".into(),
        minimum_out: "1".into(),
        output_decimals: decimals,
    };
    let quote = SwapQuote {
        from: qoge.clone(),
        to: usdt_asset.clone(),
        amount_in: "10000000000000000000000".into(),
        amount_out: "30081".into(),
        minimum_out: "29482".into(),
        slippage_bps: 50,
        path: vec![],
        route: vec![],
        pools: vec![],
        impact_bps: 1,
        fee_bps: 60,
        router: "0x00".into(),
        allowance: None,
        approval_needed: false,
        balance: None,
        insufficient: false,
        warnings: vec![],
        observed_at: 0,
        liquidity_at: None,
        legs: vec![
            leg(Venue::LaunchAmm, vec!["0x0048848ca70ea1560577b4725a84b23b6bc589e2".into(), wquai.clone()], &["QOGE", "WQUAI"], 18),
            leg(Venue::Main, vec![wquai.clone(), usdt.clone()], &["WQUAI", "USDT"], 6),
        ],
    };
    app.eco.swap.from = qoge;
    app.eco.swap.to = Some(usdt_asset);
    app.eco.swap.amount = "10000".into();
    app.eco.swap.quote = Some(Ok(quote));
    app.eco.swap.quote_key = 1;
    app.eco.swap.requested_key = 1;
    app.eco.swap.requested_input = app.swap_input_key();
    app.eco.swap.quoted_at = Some(Instant::now());
    app.swap_submit();
    // The route is one durable core intent: the first swap ends on the hub, and the second is
    // sized from that swap's own receipt rather than from the quote.
    match &app.eco.flow.as_ref().expect("sequence started").kind {
        FlowKind::Steps { prepare, .. } => {
            let Prepare::Trading { intent } = prepare.as_ref() else { panic!("{prepare:?}") };
            assert_eq!(intent.account, owner);
            assert!(
                matches!(
                    &intent.action,
                    wallet_core::execution::TradingAction::CrossVenue { from, to, hub, stage: 0, .. }
                        if from == &qoge_address && to == &usdt && hub == &wquai
                ),
                "{:?}",
                intent.action
            );
        }
        other => panic!("{other:?}"),
    }
    // The first swap is signed: the sequence waits for it rather than finishing.
    app.on_event(review("s1", "swap"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    app.on_event(submitted("s1"), size);
    assert!(matches!(app.modal, Modal::None), "no result dialog between the two swaps");
    let flow = app.eco.flow.as_ref().expect("still running");
    assert_eq!(flow.waiting.as_deref(), Some("s1"));
    // Confirmed, but its output not recorded yet: nothing is asked for.
    app.dash.ops = vec![op("s1", "swap", OpStatus::Confirmed)];
    app.advance_flow();
    assert!(!app.eco.flow.as_ref().unwrap().requested, "no second review before the first's output is known");
    // The receipt says it paid 61.25 WQUAI, in units it names and to the token the route saved:
    // that is the second swap, on to USDT, sized from the receipt rather than from the quote.
    let mut paid = op("s1", "swap", OpStatus::Confirmed);
    paid.account = owner.clone();
    paid.detail = serde_json::json!({"actual_out": "61250000000000000000", "to_decimals": 18, "to_token": wquai});
    app.dash.ops = vec![paid];
    app.advance_flow();
    let flow = app.eco.flow.as_ref().unwrap();
    assert!(flow.requested, "the second review is asked for");
    match &flow.kind {
        FlowKind::Steps { prepare, .. } => {
            let Prepare::Trading { intent } = prepare.as_ref() else { panic!("{prepare:?}") };
            assert!(
                matches!(
                    &intent.action,
                    wallet_core::execution::TradingAction::CrossVenue { from, to, amount, stage: 1, .. }
                        if from == &wquai && to == &usdt && amount == "61.25"
                ),
                "{:?}",
                intent.action
            );
        }
        other => panic!("{other:?}"),
    }
    // The second swap finishes the route.
    app.on_event(review("s2", "swap"), size);
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    app.on_event(submitted("s2"), size);
    assert!(app.eco.flow.is_none());
    assert!(matches!(app.modal, Modal::Result(_)));
}

/// Markets lists every venue, but Pools only offers the main exchange (the only one liquidity
/// is added through), and trading a curve opens its buy form instead of the swap card.
#[test]
fn markets_hold_every_venue_and_pools_hold_the_two_that_mint_lp() {
    use wallet_core::markets::{CurveMark, Pool, PoolToken, Venue};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let wquai = wallet_core::sdk::wrappers::WQUAI_MAINNET_ADDRESS;
    let mut pools = pool_shape();
    pools.push(Pool {
        address: "0x00qoge".into(),
        token0: PoolToken { address: "0x00q".into(), symbol: "QOGE".into(), decimals: 18 },
        token1: PoolToken { address: wquai.into(), symbol: "WQUAI".into(), decimals: 18 },
        reserve0: 1.0,
        reserve1: 1.0,
        venue: Venue::LaunchAmm,
        ..Default::default()
    });
    pools.push(Pool {
        address: "0x00curve".into(),
        token0: PoolToken { address: "0x00c".into(), symbol: "CHEEZ".into(), decimals: 18 },
        token1: PoolToken { address: wquai.into(), symbol: "WQUAI".into(), decimals: 18 },
        venue: Venue::Curve,
        curve: Some(CurveMark {
            venue_kind: Some(wallet_core::capabilities::Family::QuainanceCurve),
            price_basis: Default::default(),
            price_quai: Some(0.00007),
            raised_quai: 1.0,
            target_quai: Some(25_000.0),
            progress_bps: Some(10),
            launchpad: None,
        }),
        ..Default::default()
    });
    app.eco.markets_view.pools = Some(Ok((pools, wallet_core::markets::DexOverview::default())));
    assert_eq!(app.directory_rows().len(), 5, "every pool that has an LP token: main and launch AMM");
    // Both exchanges that hold LP; only the curves are excluded, having no LP token to stake.
    assert!(app.directory_rows().iter().all(|p| matches!(p.venue, Venue::Main | Venue::LaunchAmm)));
    assert!(!app.directory_rows().iter().any(|p| p.venue == Venue::Curve), "a bonding curve is not a pool");
    // The picker can reach the graduated token, never the curve.
    let graph = app.route_graph();
    assert!(graph.routable(wquai, "0x00q"));
    assert!(!graph.has_pool("0x00c"));
    // `t` on the curve row opens its buy form.
    app.switch(Screen::Markets);
    app.selected = 5;
    press(&mut app, KeyCode::Char('t'));
    match &app.modal {
        Modal::Form(form) => {
            assert!(matches!(&form.kind, FormKind::CurveBuy { symbol, curve, .. } if symbol == "CHEEZ" && curve == "0x00curve"))
        }
        _ => panic!("the curve buy form opens"),
    }
}

/// Settings › IPFS gateway: a gateway that cannot be reached is not saved; one that answers is
/// tested, saved to config.toml and used at once; an empty field goes back to ipfs.io.
#[test]
fn the_ipfs_gateway_is_tested_before_it_is_saved() {
    use super::SETTINGS;
    let _gateway = crate::tui::IPFS_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // A stand-in node that answers, but does not have the test file (a node just started).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming().flatten().take(4) {
            let mut stream = stream;
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n");
        }
    });
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Settings);
    app.selected = SETTINGS.iter().position(|s| s.0 == "ipfs").expect("a settings row");
    let submit = |app: &mut App, url: &str| {
        press(app, KeyCode::Enter);
        let Modal::Form(mut form) = std::mem::replace(&mut app.modal, Modal::None) else { panic!("gateway form") };
        assert!(matches!(form.kind, FormKind::IpfsGateway(wallet_core::ipfs::Content::Media)));
        form.fields[0].value = url.into();
        app.submit_form(&form);
        let started = std::time::Instant::now();
        while app.ipfs_check.is_some() && started.elapsed() < std::time::Duration::from_secs(15) {
            app.poll_ipfs_check();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        app.toasts.last().map(|t| t.text.clone()).unwrap_or_default()
    };
    // Nothing listening: refused, and nothing changes.
    let toast = submit(&mut app, "http://127.0.0.1:1");
    assert!(toast.contains("not saved"), "{toast}");
    assert_eq!(app.config.ipfs_gateway, None);
    // A public gateway over plain http is refused before anything is fetched.
    let toast = submit(&mut app, "http://gateway.example");
    assert!(toast.contains("https"), "{toast}");
    // A node that answers is saved, with the reason it could not be fully verified.
    let url = format!("http://localhost:{port}");
    let toast = submit(&mut app, &url);
    assert!(toast.contains("saved") && toast.contains("did not arrive"), "{toast}");
    assert_eq!(app.config.ipfs_gateway.as_deref(), Some(format!("http://127.0.0.1:{port}").as_str()), "stored as it is used");
    assert_eq!(
        wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Media).display(),
        format!("http://127.0.0.1:{port}"),
        "and in use at once"
    );
    assert!(
        wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Abi).is_default_for(wallet_core::ipfs::Content::Abi),
        "the ABI gateway is a separate setting and was not touched"
    );
    app.flush_config();
    assert_eq!(AppConfig::load(&app.paths).unwrap().ipfs_gateway, app.config.ipfs_gateway, "written to config.toml");
    // Empty goes back to the built-in gateway, with no fetch: there is nothing to test about a
    // default the wallet ships, and it must work while the configured node is unreachable.
    let toast = submit(&mut app, "");
    assert!(toast.contains(wallet_core::ipfs::DEFAULT_MEDIA_GATEWAY), "{toast}");
    assert_eq!(app.config.ipfs_gateway, None);
    assert!(wallet_core::ipfs::gateway(wallet_core::ipfs::Content::Media).is_default_for(wallet_core::ipfs::Content::Media));
}

/// System › Wallets shows every wallet's worth without unlocking any: the last priced summary,
/// carried forward by how much its QUAI has changed since, and a total across all of them.
#[test]
fn the_wallet_cockpit_adds_up_every_wallet() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::portfolio::{AssetKey, Portfolio};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let other = app.registry.create_watch("savings", &[("0x0011111111111111111111111111111111111111".into(), "Cold".into())]).unwrap();
    let e18 = |n: u128| (n * 10u128.pow(18)).to_string();
    let mut quai = asset_row(AssetKey::Quai, "QUAI", &e18(100), true);
    quai.price_usd = Some(0.5);
    quai.value_usd = Some(50.0);
    let priced = |rows| Portfolio {
        network: app.network_id.clone(),
        rows,
        total_usd: 50.0,
        observed_at: wallet_core::registry::now(),
        ..Default::default()
    };
    wallet_core::cockpit::save_summary(&app.paths, &other.id, &priced(vec![quai.clone()]));
    let mine = app.meta.as_ref().unwrap().id.clone();
    wallet_core::cockpit::save_summary(&app.paths, &mine, &priced(vec![quai.clone()]));
    app.eco.portfolio = Some(priced(vec![quai]));
    app.switch(Screen::Wallets);
    app.load_wallets();
    // The savings wallet has since received 20 QUAI: at $0.50 that is $10 more.
    app.wallet_quai.insert(other.id.clone(), wallet_core::sdk::U256::from(120u128 * 10u128.pow(18)));
    let mut term = Terminal::new(TestBackend::new(160, 20)).unwrap();
    term.draw(|f| super::super::ui::draw(f, &mut app)).unwrap();
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains("2 · $110.00 together"), "total of $50 and $60");
    assert!(text.contains("$60.00") && text.contains("120"), "live QUAI moves the savings wallet's value");
}

/// On Markets, `A` opens an alert form for the pair under the cursor, priced the way the pair is
/// shown; a watched pair moves to the top and the cursor follows the pair it was on.
#[test]
fn markets_alerts_and_watching() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.switch(Screen::Markets);
    app.pane = 0;
    app.selected = 1;
    let second = app.selected_pool().unwrap();
    sheet(&mut app, 'a');
    let Modal::Form(f) = &app.modal else { panic!("an alert form") };
    let FormKind::Alert { pool, name, .. } = &f.kind else { panic!("{:?}", f.kind) };
    assert_eq!(pool, &second.address);
    assert_eq!(name, &app.pair_name(&second));
    assert!(f.fields[1].value.parse::<f64>().is_ok_and(|v| v > 0.0), "starts at the price now: {:?}", f.fields[1].value);
    app.modal = Modal::None;
    // Watching the second pair lifts it to the top, and the cursor goes with it.
    app.eco.watchlist = vec![second.address.clone()];
    app.keep_cursor_on(Some(second.address.clone()));
    assert_eq!(app.selected_pool().map(|p| p.address), Some(second.address.clone()));
    assert_eq!(app.selected, 0);
}

/// A send review turns into the command that prepares the same send.
#[test]
fn a_send_review_copies_as_a_command() {
    let review = |kind: &str, to: &str, amount: &str, fields: Vec<wallet_core::tx::Field>| wallet_core::tx::Review {
        op_id: "x".into(),
        kind: kind.into(),
        title: String::new(),
        network: String::new(),
        from: "0x00aa (Main)".into(),
        to: to.into(),
        asset: String::new(),
        amount: amount.into(),
        amount_base: String::new(),
        max_fee: String::new(),
        fee_bps: None,
        fields,
        coins: vec![],
        warnings: vec![],
        visuals: vec![],
        fee_over_policy: false,
        changes: vec![],
    };
    assert_eq!(
        review_cli(&review("send_quai", "0x00bb", "1,250.5 QUAI", vec![])).as_deref(),
        Some("quai-terminal send quai --to 0x00bb --amount 1250.5 --from 0x00aa")
    );
    let field = |label: &str, value: &str| wallet_core::tx::Field { label: label.into(), value: value.into() };
    let token = review("send_token", "0x00cc", "3 WQI", vec![field("Token contract", "0x00cc"), field("Call", "transfer(0x00dd, 3000)")]);
    assert_eq!(review_cli(&token).as_deref(), Some("quai-terminal send token 0x00cc --to 0x00dd --amount 3 --from 0x00aa"));
    assert!(review_cli(&review("swap", "0x00ee", "1 WQI", vec![])).is_none());
}

/// A pinned chat docks beside every screen but the Board, and ` writes to it from anywhere.
#[test]
fn a_pinned_chat_docks_beside_every_screen() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let post = |at: u64, text: &str| wallet_core::messages::Post {
        at,
        timed: true,
        block: at,
        tx: String::new(),
        index: 0,
        from: "0x0011111111111111111111111111111111111111".into(),
        tag: String::new(),
        kind: wallet_core::messages::KIND_TEXT,
        body: text.as_bytes().to_vec(),
    };
    app.eco.board.pin = Some("#trading".into());
    app.eco.board.subs = vec!["#trading".into()];
    app.eco.board.posts.insert("trading".into(), Ok(vec![post(20, "wqi pumping"), post(10, "gm")]));
    let screen = |app: &mut App, w: u16, h: u16| {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    app.switch(Screen::Markets);
    let wide = screen(&mut app, 180, 40);
    assert!(wide.contains("#trading · ●") && wide.contains("tab or ` to write") && wide.contains("wqi pumping"), "a column beside Markets");
    assert!(wide.find("gm").unwrap() < wide.find("wqi pumping").unwrap(), "newest at the bottom");
    assert!(screen(&mut app, 120, 40).contains("wqi pumping"), "a strip along the bottom when narrower");
    // A long message wraps under its sender instead of being cut, in either dock shape.
    let long =
        "the WQI pool on Quainance just took a very large buy and the gauge rewards doubled overnight, worth a look before it settles";
    app.eco.board.posts.insert("trading".into(), Ok(vec![post(30, long), post(20, "wqi pumping"), post(10, "gm")]));
    for (w, h) in [(180, 40), (120, 40)] {
        let text = screen(&mut app, w, h);
        let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for word in ["overnight,", "settles"] {
            assert!(flat.contains(word), "{word} shows at {w}x{h}");
        }
    }
    app.switch(Screen::Board);
    assert!(!screen(&mut app, 180, 40).contains("` to write"), "the Board shows the chat itself");
    app.switch(Screen::Home);
    press(&mut app, KeyCode::Char('`'));
    assert!(matches!(&app.modal, Modal::Form(f) if f.kind == FormKind::BoardPost { channel: "trading".into() }), "` writes to it");
}

/// Dock wrapping: whole words per line, the first line shorter, and a word too long for any line
/// split rather than lost.
#[test]
fn dock_messages_wrap_by_word() {
    let lines = super::super::views::wrap_words("gm frens the pool is deep today", 10, 14);
    assert_eq!(lines, ["gm frens", "the pool is", "deep today"]);
    assert!(lines.iter().enumerate().all(|(i, l)| l.chars().count() <= if i == 0 { 10 } else { 14 }));
    let address = "send to 0x004dd9AFAA2768642B5cDe15c24F37bF19d842E4 please";
    let lines = super::super::views::wrap_words(address, 12, 12);
    assert_eq!(lines.concat().replace(' ', ""), address.replace(' ', ""), "nothing lost");
    assert!(lines.iter().all(|l| l.chars().count() <= 12), "{lines:?}");
    assert_eq!(super::super::views::wrap_words("", 10, 10), [""]);
}

/// Tab reaches the pinned chat after the screen's last pane; typing there goes to the message
/// box (digits included, not section switches), Enter hands it to the usual post review, and
/// Tab or Esc return to the screen.
#[test]
fn tab_into_the_pinned_chat_and_post() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.eco.board.pin = Some("#trading".into());
    app.eco.board.posts.insert("trading".into(), Ok(vec![]));
    app.switch(Screen::Home);
    let draw = |app: &mut App| {
        let mut term = Terminal::new(TestBackend::new(180, 40)).unwrap();
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    assert!(draw(&mut app).contains("tab or ` to write"));
    // Home has two panes: the first Tab moves between them, the next reaches the chat.
    press(&mut app, KeyCode::Tab);
    assert!(!app.dock_focus && app.pane == 1);
    press(&mut app, KeyCode::Tab);
    assert!(app.dock_focus, "Tab from the last pane goes to the chat");
    for c in "gm 2 all".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    assert_eq!(app.dock_draft, "gm 2 all");
    assert_eq!(app.screen, Screen::Home, "2 was typed, not a switch to Trade");
    let focused = draw(&mut app);
    assert!(focused.contains("› gm 2 all▏"));
    assert!(focused.contains("▸ #trading") && !focused.contains("▸ recent activity"), "one lit panel: the chat");
    assert_eq!(app.pane, 1, "the screen keeps its pane underneath");
    // Esc leaves with the draft kept; ` comes straight back.
    press(&mut app, KeyCode::Esc);
    assert!(!app.dock_focus && app.dock_draft == "gm 2 all");
    press(&mut app, KeyCode::Char('`'));
    assert!(app.dock_focus);
    press(&mut app, KeyCode::Enter);
    let Modal::Form(f) = &app.modal else { panic!("the post goes to its form and review") };
    assert_eq!(f.kind, FormKind::BoardPost { channel: "trading".into() });
    assert!(f.fields.iter().any(|fl| fl.value == "gm 2 all"));
    assert!(f.pending, "submitted for review");
    assert!(app.dock_draft.is_empty(), "the box empties once it is sent for review");
    // Tab from the chat goes round to the screen's first pane.
    app.modal = Modal::None;
    press(&mut app, KeyCode::Tab);
    assert!(!app.dock_focus && app.pane == 0);
    // Markets routes Tab itself (pairs, then flow): the chat comes after the flow.
    app.switch(Screen::Markets);
    draw(&mut app);
    press(&mut app, KeyCode::Tab);
    assert!(!app.dock_focus && app.pane == 1);
    press(&mut app, KeyCode::Tab);
    assert!(app.dock_focus, "after Markets' flow pane");
    press(&mut app, KeyCode::Tab);
    assert!(!app.dock_focus && app.pane == 0, "and back to the pairs");
    // The swap card: from its last field, not while it is being entered.
    app.switch(Screen::Swap);
    draw(&mut app);
    press(&mut app, KeyCode::Tab);
    assert!(!app.dock_focus && app.eco.swap.field == 1, "Tab first enters the card");
    app.eco.swap.field = 4;
    press(&mut app, KeyCode::Tab);
    assert!(app.dock_focus, "from the card's last field");
}

/// Announced senders are offers above the registered channels: enter asks before registering
/// one (announcements are unauthenticated), x declines, and the channel keys still reach the
/// channels below them.
#[test]
fn channel_offers_are_accepted_only_after_asking() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let (worker, mut sent) = Worker::capture();
    app.worker = Some(worker);
    app.dash.unlocked = true;
    let offer = |code: &str, qits: u64| wallet_core::ops::ChannelOffer {
        code: code.into(),
        found: wallet_core::sdk::U256::from(qits),
        first_seen: 1,
        last_probe: 1,
        notified: true,
    };
    app.dash.offers = vec![offer("PM8Toffered", 2_500), offer("PM8Tdust", 3)];
    app.dash.peers = vec![wallet_core::ops::PeerView { code: "PM8Tpeer".into(), contact: None, receive_addresses: 1, send_addresses: 0 }];
    app.switch(Screen::Channels);
    while sent.try_recv().is_ok() {}
    app.selected = 0;
    assert_eq!(app.channel_offer().map(|o| o.code.as_str()), Some("PM8Toffered"));
    assert!(app.channel_peer().is_none(), "an offer is not a channel");
    press(&mut app, KeyCode::Char('s'));
    assert!(matches!(&app.modal, Modal::Confirm { action: ConfirmAction::AcceptOffer(c), .. } if c == "PM8Toffered"), "asks first");
    assert!(sent.try_recv().is_err(), "nothing registered yet");
    press(&mut app, KeyCode::Char('y'));
    assert!(matches!(sent.try_recv(), Ok(Cmd::AcceptOffer(c)) if c == "PM8Toffered"));
    app.selected = 1;
    press(&mut app, KeyCode::Char('x'));
    assert!(sent.try_recv().is_err(), "declining is for good, so it asks first");
    press(&mut app, KeyCode::Char('y'));
    assert!(matches!(sent.try_recv(), Ok(Cmd::DeclineOffer(c)) if c == "PM8Tdust"));
    // Below the offers, the registered channel: s pays it, R rescans it, x leaves it alone.
    app.selected = 2;
    assert_eq!(app.channel_peer().map(|p| p.code.as_str()), Some("PM8Tpeer"));
    press(&mut app, KeyCode::Char('R'));
    assert!(matches!(sent.try_recv(), Ok(Cmd::ScanPeer(c)) if c == "PM8Tpeer"));
    press(&mut app, KeyCode::Char('x'));
    assert!(sent.try_recv().is_err(), "x never touches a registered channel");
}

/// A refresh that adds, removes or reorders offers keeps the cursor on the sender it was on, so
/// a key pressed just after never lands on someone else.
#[test]
fn the_channels_cursor_follows_the_sender_across_a_refresh() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let (worker, mut sent) = Worker::capture();
    app.worker = Some(worker);
    app.dash.unlocked = true;
    let offer = |code: &str, first_seen: u64| wallet_core::ops::ChannelOffer {
        code: code.into(),
        found: wallet_core::sdk::U256::from(2_000u64),
        first_seen,
        last_probe: first_seen,
        notified: true,
    };
    app.dash.offers = vec![offer("PM8Talice", 1), offer("PM8Tspam", 2)];
    app.switch(Screen::Channels);
    while sent.try_recv().is_ok() {}
    app.selected = 0;
    // A new offer arrives ahead of the one under the cursor.
    let mut d = app.dash.clone();
    d.offers = vec![offer("PM8Tnew", 0), offer("PM8Talice", 1), offer("PM8Tspam", 2)];
    app.on_event(Ev::Dashboard(Box::new(d)), (120, 40));
    assert_eq!(app.channel_offer().map(|o| o.code.as_str()), Some("PM8Talice"));
    press(&mut app, KeyCode::Char('x'));
    press(&mut app, KeyCode::Char('y'));
    assert!(matches!(sent.try_recv(), Ok(Cmd::DeclineOffer(c)) if c == "PM8Talice"));
}

/// A feature that is off is gone from the sidebar, the tabs, the palette, the hints and every
/// way in; nothing polls for it; and its Settings row brings it back.
#[test]
fn a_feature_turned_off_is_hidden_everywhere_and_settings_brings_it_back() {
    use super::super::palette::Run;
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.features = wallet_core::config::Features { messaging: false, trading: false, nfts: false };
    let screen = |app: &mut App, w: u16, h: u16| {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    assert!(!app.sections().contains(&Section::Nfts), "a section with nothing left is not in the sidebar");
    assert_eq!(Section::Trade.screens(&app.config.features), vec![Screen::Convert], "the exchange stays, for converting and wrapping");
    assert!(!app.sections().contains(&Section::Markets), "Markets is all trading");
    assert_eq!(Section::People.screens(&app.config.features), vec![Screen::Contacts, Screen::Channels]);
    for shut in [Screen::Board, Screen::Swap, Screen::Markets, Screen::Launches, Screen::Collected, Screen::Listings] {
        app.switch(shut);
        assert_eq!(app.screen, Screen::Home, "{shut:?} stays shut");
    }
    press(&mut app, KeyCode::Char('4'));
    assert_eq!(app.screen, Screen::Home, "4 (NFTs) has nothing to open");
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.screen, Screen::Home, "2 (Markets) has nothing to open");
    press(&mut app, KeyCode::Char('t'));
    assert_eq!(app.screen, Screen::Home, "t does not open the swap card");
    assert!(!context_hints(&app).iter().any(|(_, what)| what == "swap" || what == "trade"), "and is not offered");
    press(&mut app, KeyCode::Char('3'));
    assert_eq!(app.screen, Screen::Convert, "Trade opens on what is left");
    app.switch(Screen::Wrap);
    assert_eq!(app.screen, Screen::Wrap, "wrapping stays too, behind the same tab");
    for query in ["swap", "board", "nft", "listings", "markets"] {
        let offered: Vec<String> = app
            .palette_entries(query)
            .into_iter()
            .filter(|e| matches!(e.run, Run::Go(_) | Run::Action(_) | Run::Swap { .. } | Run::Market(_)))
            .filter(|e| ["Swap", "Board", "NFT", "Listings", "Markets"].iter().any(|w| e.label.contains(w)))
            .map(|e| e.label)
            .collect();
        assert!(offered.is_empty(), "the palette offers nothing turned off for `{query}`: {offered:?}");
    }
    // A pinned chat neither docks nor polls with messaging off.
    app.eco.board.pin = Some("#general".into());
    app.switch(Screen::Home);
    assert!(!screen(&mut app, 180, 40).contains("` to write"));
    app.tick_chat();
    assert!(!app.eco.board.chat_loaded, "no chat is read");
    // Settings turns NFTs back on, saves it, and the section returns.
    app.switch(Screen::Settings);
    app.selected = SETTINGS.iter().position(|(id, _)| *id == "feature:nfts").unwrap();
    press(&mut app, KeyCode::Enter);
    assert!(app.config.features.nfts);
    app.flush_config();
    assert!(AppConfig::load(&app.paths).unwrap().features.nfts, "written to config.toml, where the daemon reads it");
    assert!(app.sections().contains(&Section::Nfts));
    app.switch(Screen::Collected);
    assert_eq!(app.screen, Screen::Collected);
}

/// Settings is longer than a small terminal: the list follows the cursor to the last row.
#[test]
fn every_setting_can_be_reached_on_a_small_terminal() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Settings);
    app.selected = SETTINGS.len() - 1;
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    term.draw(|f| super::super::ui::draw(f, &mut app)).unwrap();
    let text: String = term.backend().buffer().content().iter().map(|c| c.symbol()).collect();
    assert!(text.contains(SETTINGS[SETTINGS.len() - 1].1), "the last row is on screen once selected");
}

/// `L` orders the pairs by depth and `M` by how far they moved today, each cycling back to the
/// directory's own order; a watched pair stays at the top of every order, and the cursor keeps
/// the pair it was on.
#[test]
fn markets_sort_by_tvl_and_movement_with_watched_pinned() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    // A day-ago price on two pairs: one up, one down.
    if let Some(Ok((pools, _))) = &mut app.eco.markets_view.pools {
        pools[1].spot_24h_ago = Some(0.5); // SMOL/WQI: up
        pools[2].spot_24h_ago = Some(2.0); // LAPTOP/WQUAI: down
    }
    app.switch(Screen::Markets);
    app.pane = 0;
    let names = |app: &App| app.market_rows().iter().map(|p| p.address.clone()).collect::<Vec<_>>();
    let unsorted = names(&app);
    sheet(&mut app, 'l');
    let by_tvl = names(&app);
    assert_eq!(by_tvl.first(), Some(&"0x00pairWQIWQUAI".to_string()), "deepest first");
    assert_eq!(by_tvl.last(), Some(&"0x00pairNVNTWQUAI".to_string()), "shallowest last");
    sheet(&mut app, 'l');
    assert_eq!(names(&app).first(), Some(&"0x00pairNVNTWQUAI".to_string()), "pressed again: shallowest first");
    sheet(&mut app, 'l');
    assert_eq!(names(&app), unsorted, "and again: back to the directory's order");
    sheet(&mut app, 'c');
    assert_eq!(names(&app).first(), Some(&"0x00pairSMOLWQI".to_string()), "biggest gainer first");
    sheet(&mut app, 'c');
    assert_eq!(names(&app).first(), Some(&"0x00pairLAPTOPWQUAI".to_string()), "pressed again: biggest loser first");
    // Watching the shallowest pair pins it above everything, whatever the order.
    app.eco.watchlist = vec!["0x00pairNVNTWQUAI".into()];
    assert_eq!(names(&app).first(), Some(&"0x00pairNVNTWQUAI".to_string()), "watched stays on top");
    sheet(&mut app, 'l');
    assert_eq!(names(&app).first(), Some(&"0x00pairNVNTWQUAI".to_string()), "still on top under another order");
    assert_eq!(names(&app)[1], "0x00pairWQIWQUAI", "the rest follow the order that was asked for");
    // The cursor follows the pair it was on.
    app.selected = 2;
    let holding = app.selected_pool().unwrap().address;
    app.keep_cursor_on(Some(holding.clone()));
    assert_eq!(app.selected_pool().map(|p| p.address), Some(holding));
}

/// Enter on a page that is already open does not stack another copy of it, so one Esc goes back.
#[test]
fn opening_the_page_you_are_on_does_not_stack_it() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Home);
    app.push_detail(Detail::Asset("quai".into()));
    for _ in 0..4 {
        app.enter_eco();
    }
    assert_eq!(app.detail.len(), 1, "one page, however many times enter is pressed");
    app.push_detail(Detail::Asset("qi".into()));
    assert_eq!(app.detail.len(), 2, "a different page still opens");
}

/// On an asset's page, `b` and `S` open the swap card already pointing the right way: buying pays
/// QUAI for the asset, selling does the reverse, and QUAI itself is traded against WQI.
#[test]
fn an_asset_page_can_buy_and_sell_it() {
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let smol = "0x00a1";
    app.switch(Screen::Home);
    app.push_detail(Detail::Asset(smol.into()));
    press(&mut app, KeyCode::Char('b'));
    assert_eq!(app.screen, Screen::Swap);
    assert!(matches!(app.eco.swap.from, SwapAsset::Quai), "buying pays QUAI");
    assert!(matches!(&app.eco.swap.to, Some(SwapAsset::Token { address, .. }) if address == smol));
    assert!(app.detail.is_empty(), "the card is the screen now, not a page over it");
    app.push_detail(Detail::Asset(smol.into()));
    press(&mut app, KeyCode::Char('S'));
    assert!(matches!(&app.eco.swap.from, SwapAsset::Token { address, .. } if address == smol), "selling pays the asset");
    assert!(matches!(app.eco.swap.to, Some(SwapAsset::Quai)));
    // QUAI is traded against WQI, never against itself.
    app.push_detail(Detail::Asset("quai".into()));
    press(&mut app, KeyCode::Char('b'));
    assert!(matches!(&app.eco.swap.from, SwapAsset::Token { symbol, .. } if symbol == "WQI"));
    assert!(matches!(app.eco.swap.to, Some(SwapAsset::Quai)));
    // With trading off the keys do nothing at all.
    app.config.features.trading = false;
    app.switch(Screen::Home);
    app.push_detail(Detail::Asset(smol.into()));
    press(&mut app, KeyCode::Char('b'));
    assert_eq!(app.screen, Screen::Home, "no swap card");
}

/// Sorting or watching must never move the cursor off the pair the chart is on, and must never
/// reach into the flow column's cursor while that column has it.
#[test]
fn sorting_keeps_the_chart_and_the_cursor_on_the_same_pair() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.switch(Screen::Markets);
    app.pane = 0;
    // The row the list highlights, exactly as the view computes it.
    let highlighted = |app: &App| {
        let rows = app.market_rows();
        rows.get(app.markets_pair().min(rows.len().saturating_sub(1))).map(|p| p.address.clone())
    };
    app.selected = 2;
    let held = highlighted(&app).unwrap();
    for key in ['L', 'L', 'M', 'M', 'L'] {
        press(&mut app, KeyCode::Char(key));
        assert_eq!(highlighted(&app), Some(held.clone()), "after {key}: the cursor left the pair it was on");
        assert_eq!(app.selected_pool().map(|p| p.address), highlighted(&app), "after {key}: the chart and the cursor disagree");
    }
    // Watching re-orders the list; the cursor still holds its pair.
    app.eco.watchlist = vec![app.market_rows()[0].address.clone()];
    app.keep_cursor_on(Some(held.clone()));
    assert_eq!(app.selected_pool().map(|p| p.address), Some(held.clone()));

    // With the cursor in the flow column, sorting the pairs must not touch it.
    app.pane = 1;
    app.selected = 0;
    app.eco.markets_view.flow_selected = 0;
    sheet(&mut app, 'l');
    assert_eq!(app.selected, 0, "sorting the pairs moved the flow column's cursor");
    assert_eq!(app.selected_pool().map(|p| p.address), Some(held), "and the chart still holds its pair");
}

/// What the Markets screen actually draws: the chart is the pair the cursor is on, whatever order
/// the list is in. The chart used to be read out of the unordered directory by the sorted list's
/// row number, so sorting or watching a pair pointed it at a different pair than the highlight.
#[test]
fn the_chart_draws_the_pair_the_cursor_is_on_in_every_order() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.switch(Screen::Markets);
    app.pane = 0;
    let mut term = Terminal::new(TestBackend::new(160, 44)).unwrap();
    let drawn = |app: &mut App, term: &mut Terminal<TestBackend>| -> String {
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        let b = term.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    };
    // In every order, and with a pair watched, the chart names the pair under the cursor.
    for (key, watch) in [(None, false), (Some('l'), false), (Some('c'), false), (Some('l'), true)] {
        if watch {
            app.eco.watchlist = vec![app.market_rows().last().unwrap().address.clone()];
        }
        if let Some(k) = key {
            sheet(&mut app, k);
        }
        app.selected = 2;
        app.eco.markets_view.pair_selected = 2;
        let pool = app.selected_pool().unwrap();
        let base0 = app.pool_base0(&pool);
        let (base, quote) = if base0 { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
        let name = format!("{}/{}", app.market_symbol(base), app.market_symbol(quote));
        let screen = drawn(&mut app, &mut term);
        // The chart's own title line, not just the name appearing somewhere in the pairs list.
        let title = screen
            .lines()
            .find(|l| l.contains(". timeframe"))
            .unwrap_or_else(|| panic!("no chart title (key {key:?}, watched {watch})"))
            .to_string();
        assert!(title.contains(&name), "the chart is on a different pair than the cursor: wanted {name} in {title:?}");
    }
}

/// An asset's page names `t` for trading in its footer, and its action sheet carries buying and
/// selling; with trading off, none of the three is offered.
#[test]
fn an_assets_page_names_the_trade_key_beside_buy_and_sell() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    for id in ["quai", "0x00token"] {
        app.detail = vec![Detail::Asset(id.into())];
        let hints = context_hints(&app);
        assert_eq!(hints.iter().find(|(k, _)| k == "t").map(|(_, w)| w.as_str()), Some("trade"), "{id}: {hints:?}");
        app.open_sheet();
        let Modal::Sheet { items, .. } = &app.modal else { panic!("{id}: a sheet") };
        let letters: Vec<char> = items.iter().map(|i| i.key).collect();
        assert!(letters.contains(&'b') && letters.contains(&'S'), "{id}: buy and sell in the sheet: {letters:?}");
        app.modal = Modal::None;
    }
    // Turning trading off takes all three away rather than leaving a key that does nothing.
    app.config.features.trading = false;
    app.detail = vec![Detail::Asset("quai".into())];
    assert!(!context_hints(&app).iter().any(|(k, _)| k == "t"));
    app.open_sheet();
    let Modal::Sheet { items, .. } = &app.modal else { panic!("a sheet") };
    assert!(!items.iter().any(|i| matches!(i.key, 'b' | 'S')), "no buying or selling");
}

/// The call form changes shape with the function picked: each one brings its own arguments, a
/// payable one gains a QUAI field, and what was typed into the account survives the change.
#[test]
fn the_contract_call_form_follows_the_function_picked() {
    use wallet_core::contracts::Callable;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    // With an account present, "From" is a choice field — which is the precondition for a rebuild
    // being triggered by moving onto the function picker rather than by changing it.
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::ZERO,
        locked: U256::ZERO,
        nonce: 0,
    }];
    let functions = vec![
        Callable { signature: "deposit()".into(), name: "deposit".into(), inputs: vec![], payable: true, read_only: false },
        Callable {
            signature: "transfer(address,uint256)".into(),
            name: "transfer".into(),
            inputs: vec![("to".into(), "address".into()), ("amount".into(), "uint256".into())],
            payable: false,
            read_only: false,
        },
    ];
    app.open_form(FormKind::ContractCall { address: "0x0077ad436f63f35d0ded89055402659750a28d0a".into(), name: "Vault".into(), functions });
    let labels = |app: &App| match &app.modal {
        Modal::Form(f) => f.fields.iter().map(|x| x.label.clone()).collect::<Vec<_>>(),
        _ => panic!("no form"),
    };
    assert!(matches!(&app.modal, Modal::Form(f) if f.title.contains("Vault")));
    // The first function is payable and takes nothing.
    assert_eq!(labels(&app), ["From", "Function", "QUAI to send"]);
    // Whatever is in the account field must survive a change of function.
    let account = match &app.modal {
        Modal::Form(f) => f.fields[0].value.clone(),
        _ => panic!("no form"),
    };
    assert!(account.starts_with("0x"), "the account field carries the chosen account: {account:?}");
    // Put the cursor on the function picker wherever it is, and pick the next function: its
    // arguments replace the QUAI field.
    let focus_function = |app: &mut App| {
        if let Modal::Form(f) = &mut app.modal {
            f.focus = f.fields.iter().position(|x| x.label == "Function").expect("a function picker");
        }
    };
    focus_function(&mut app);
    press(&mut app, KeyCode::Right);
    assert_eq!(labels(&app), ["From", "Function", "to (address)", "amount (uint256)"]);
    // And back again: the payable field returns and the arguments are gone.
    focus_function(&mut app);
    press(&mut app, KeyCode::Left);
    assert_eq!(labels(&app), ["From", "Function", "QUAI to send"]);
    let Modal::Form(form) = &app.modal else { panic!("no form") };
    assert_eq!(form.fields[0].value, account, "the account was rebuilt away");
    assert_eq!(form.fields[1].value, "deposit()");
    // Each argument field says what to type for its type.
    focus_function(&mut app);
    press(&mut app, KeyCode::Right);
    let Modal::Form(form) = &app.modal else { panic!("no form") };
    assert!(form.fields[2].hint.contains("Quai address"), "{:?}", form.fields[2].hint);
    assert!(form.fields[3].hint.contains("whole number"), "{:?}", form.fields[3].hint);

    // Typed arguments survive moving the cursor around. Only changing the function rebuilds them;
    // tabbing past the account (itself a choice field) must not.
    if let Modal::Form(f) = &mut app.modal {
        f.fields[2].value = "0x00recipient".into();
        f.fields[3].value = "42".into();
        f.focus = 0;
    }
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    let Modal::Form(form) = &app.modal else { panic!("no form") };
    assert_eq!(form.fields[2].value, "0x00recipient", "tabbing through the form erased an argument");
    assert_eq!(form.fields[3].value, "42", "tabbing through the form erased an argument");
}

/// A destination that is a contract is named on the send form before anything is signed, and the
/// note goes away when the address is edited back to something incomplete.
#[test]
fn the_send_form_says_when_the_destination_is_a_contract() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.open_form(FormKind::SendQuai);
    let address = "0x0077ad436f63f35d0ded89055402659750a28d0a";
    // The worker answers with what it found; the form picks it up for the address it asked about.
    let size = (120u16, 40u16);
    app.contract_probe = Some(address.into());
    app.on_event(
        super::super::worker::Ev::Contract {
            address: address.into(),
            found: Box::new(Some(wallet_core::contracts::Discovered {
                address: address.into(),
                code_len: 610,
                code_hash: "0x00".into(),
                solc: Some("0.8.20".into()),
                metadata: Some(
                    wallet_core::contracts::parse_metadata(
                        "QmTest",
                        &serde_json::to_vec(&serde_json::json!({
                            "settings": {"compilationTarget": {"a.sol": "Messages"}},
                            "output": {"abi": []},
                        }))
                        .unwrap(),
                    )
                    .unwrap(),
                ),
                metadata_error: None,
                undeclared: vec![],
                verified: Some(false),
            })),
        },
        size,
    );
    let note = match &app.modal {
        Modal::Form(f) => f.contract_note.clone(),
        _ => panic!("no form"),
    };
    let note = note.expect("the form says what the destination is");
    assert!(note.contains("Messages") && note.contains("contract") && note.contains("^F"), "{note}");
    // An answer for a different address is ignored rather than shown against this one.
    app.contract_probe = Some("0x00other".into());
    app.on_event(super::super::worker::Ev::Contract { address: address.into(), found: Box::new(None) }, size);
    assert_eq!(app.contract_probe.as_deref(), Some("0x00other"), "a stale answer was taken for the live one");

    // Editing the address back to something incomplete takes the note away with it. Leaving it
    // up would advertise a ^F that no longer has a contract to open.
    app.contract_probe = None;
    if let Modal::Form(f) = &mut app.modal {
        f.fields.iter_mut().find(|x| x.label == "To").unwrap().value = address.into();
        f.focus = f.fields.iter().position(|x| x.label == "To").unwrap();
    }
    press(&mut app, KeyCode::Backspace);
    let note = match &app.modal {
        Modal::Form(f) => f.contract_note.clone(),
        _ => panic!("no form"),
    };
    assert_eq!(note, None, "the note outlived the address it was about");
}

/// Onboarding walks through the connections before any wallet exists, and enter through every
/// field takes the defaults without setting anything.
#[test]
fn onboarding_walks_through_the_connections_and_enter_takes_the_defaults() {
    use super::super::onboarding;
    // Onboarding takes the keyboard itself; `press` routes to the screen handler, which steps
    // aside while it is up.
    let press = |app: &mut App, code: KeyCode| onboarding::on_key(app, KeyEvent::new(code, KeyModifiers::NONE));
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.monitor_endpoints.clear();
    app.config.abi_ipfs_gateway = None;
    app.config.ipfs_gateway = None;
    app.onboarding = Some(Onboarding::Privacy { selected: 0 });
    // Privacy leads into connections, which is step 3 of five.
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.onboarding, Some(Onboarding::Connections { .. })), "privacy did not lead into connections");
    assert_eq!(onboarding::step(app.onboarding.as_ref().unwrap()), 3);
    // Every field has a reason shown beside it.
    let fields = match &app.onboarding {
        Some(Onboarding::Connections { fields, .. }) => fields.len(),
        _ => 0,
    };
    assert_eq!(fields, onboarding::CONNECTIONS.len(), "a reason for every field");
    assert!(onboarding::CONNECTIONS.iter().all(|(_, why)| why.len() > 40), "each says what it is worth");
    // Enter through them all: nothing is stored, and the defaults are in force.
    for _ in 0..fields {
        press(&mut app, KeyCode::Enter);
    }
    assert!(matches!(app.onboarding, Some(Onboarding::Choose { .. })), "enter through the defaults did not reach the wallet step");
    assert!(app.config.monitor_endpoints.is_empty() && app.config.abi_ipfs_gateway.is_none() && app.config.ipfs_gateway.is_none());

    // Typed values are kept, and a bad one is refused with the cursor put back on it.
    app.onboarding = Some(Onboarding::Connections { fields: onboarding::connection_fields(&app), focus: 0 });
    for c in "http://10.0.0.12:9200".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    for _ in 0..3 {
        press(&mut app, KeyCode::Enter);
    }
    assert_eq!(app.config.monitor_endpoints.get(&app.network_id).map(|m| m.rpc_url.clone()), Some("http://10.0.0.12:9200".into()));

    app.onboarding = Some(Onboarding::Connections { fields: onboarding::connection_fields(&app), focus: 1 });
    for c in "not-a-url".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    match &app.onboarding {
        Some(Onboarding::Connections { focus, .. }) => assert_eq!(*focus, 1, "the cursor goes back to the field that was wrong"),
        _ => panic!("a bad gateway moved on anyway"),
    }
    assert!(app.config.abi_ipfs_gateway.is_none(), "and nothing was stored");
}

/// The connections step draws: the step indicator, every field, and the reason for the one under
/// the cursor.
#[test]
fn the_connections_step_draws_its_fields_and_reasons() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.onboarding = Some(Onboarding::Connections { fields: super::super::onboarding::connection_fields(&app), focus: 0 });
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    term.draw(|f| super::super::ui::draw(f, &mut app)).unwrap();
    let b = term.backend().buffer();
    let screen: String =
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n");
    if std::env::var("QW_SHOW").is_ok() {
        println!("{screen}");
    }
    assert!(screen.contains("connections"), "the step is named");
    for (label, _) in super::super::onboarding::CONNECTIONS {
        assert!(screen.contains(label), "{label} is not on screen");
    }
    // The reason for the focused field is shown, wrapped.
    let first_words: String = super::super::onboarding::CONNECTIONS[0].1.split_whitespace().take(4).collect::<Vec<_>>().join(" ");
    assert!(screen.replace('\n', " ").contains(&first_words), "no reason shown for the focused field");
    // The defaults are visible rather than blank, so nothing looks unset.
    assert!(screen.contains("ipfs.qu.ai"), "the gateway defaults are shown");
}

/// A refused connections field leaves the config exactly as it was. Applying the earlier fields
/// anyway would put a setting the user never confirmed on disk at the next save from anywhere.
#[test]
fn a_refused_connection_writes_nothing_at_all() {
    use super::super::onboarding;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.monitor_endpoints.clear();
    app.config.abi_ipfs_gateway = None;
    app.config.ipfs_gateway = None;
    let mut fields = onboarding::connection_fields(&app);
    fields[0].value = "http://10.0.0.12:9200".into();
    fields[2].value = "not-a-url".into();
    let refused = onboarding::apply_connections(&mut app, &fields);
    assert_eq!(refused.map_err(|(i, _)| i), Err(2), "the images gateway is the field that was wrong");
    assert!(app.config.monitor_endpoints.is_empty(), "the node was written despite the refusal");
    assert!(app.config.abi_ipfs_gateway.is_none() && app.config.ipfs_gateway.is_none());
    // With every field valid the same call applies all of them.
    fields[2].value = "http://127.0.0.1:8080".into();
    onboarding::apply_connections(&mut app, &fields).expect("all three are valid");
    assert_eq!(app.config.monitor_endpoints.get(&app.network_id).map(|m| m.rpc_url.clone()), Some("http://10.0.0.12:9200".into()));
    assert_eq!(app.config.ipfs_gateway.as_deref(), Some("http://127.0.0.1:8080"));
}

/// A contract can declare any number of arguments, so the call form is the first one whose height
/// is not known in advance. On a small terminal it must scroll to the focused field rather than
/// draw the rest off the bottom, and it must say that there is more.
#[test]
fn a_long_call_form_scrolls_to_the_focused_field() {
    use ratatui::{Terminal, backend::TestBackend};
    use wallet_core::contracts::Callable;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let inputs: Vec<(String, String)> = (0..8).map(|i| (format!("arg{i}"), "uint256".to_string())).collect();
    app.open_form(FormKind::ContractCall {
        address: "0x0077ad436f63f35d0ded89055402659750a28d0a".into(),
        name: "Wide".into(),
        functions: vec![Callable {
            signature: "many(uint256,uint256,uint256,uint256,uint256,uint256,uint256,uint256)".into(),
            name: "many".into(),
            inputs,
            payable: false,
            read_only: false,
        }],
    });
    // An 80x24 terminal: ten fields at two to three lines each cannot fit.
    let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let draw = |app: &mut App, term: &mut Terminal<TestBackend>| -> String {
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        let b = term.backend().buffer();
        (0..b.area.height).map(|y| (0..b.area.width).map(|x| b[(x, y)].symbol()).collect::<String>()).collect::<Vec<_>>().join("\n")
    };
    // An arrow followed by a count is the scroll marker; the footer hint's "tab/↑↓ fields" and
    // the status bar's "? more" are not.
    let scrolled = |s: &str| s.split(['↑', '↓']).skip(1).any(|p| p.trim_start().starts_with(|c: char| c.is_ascii_digit()));
    let focus_on = |app: &mut App, label: &str| {
        if let Modal::Form(f) = &mut app.modal {
            f.focus = f.fields.iter().position(|x| x.label == label).unwrap_or_else(|| panic!("no field {label}"));
        }
    };

    // At the top, the first argument is visible and the form says there is more below.
    focus_on(&mut app, "From");
    let top = draw(&mut app, &mut term);
    assert!(top.contains("arg0 (uint256)"), "the first argument is not on screen");
    assert!(scrolled(&top), "a form cut off at the bottom does not say so:\n{top}");

    // The last argument is off-screen from the top, and on-screen once focused.
    // The field row, not the function picker's label, which lists every argument name.
    assert!(!top.contains("arg7 (uint256)"), "the whole form fitted after all; the fixture is too small");
    focus_on(&mut app, "arg7 (uint256)");
    let bottom = draw(&mut app, &mut term);
    assert!(bottom.contains("arg7 (uint256)"), "the focused field is drawn off the bottom:\n{bottom}");
    assert!(scrolled(&bottom), "scrolled down, it does not say what is above:\n{bottom}");
    // The footer keeps its place rather than scrolling away with the fields.
    assert!(bottom.contains("esc cancel"), "the footer scrolled off:\n{bottom}");

    // A short form is unaffected: no markers, and every field on screen.
    app.open_form(FormKind::SendQuai);
    let short = draw(&mut app, &mut term);
    assert!(!scrolled(&short), "a form that fits claims to scroll:\n{short}");
    assert!(short.contains("esc cancel"));
}

/// Switching wallets while sitting on a screen must reload that screen for the new wallet.
///
/// The switch clears every cached view, but the screen itself never changes, so nothing re-opens
/// it — and at the moment of the switch there are no accounts yet to ask about. Collected showed
/// nothing at all until the user navigated away and back.
#[test]
fn switching_wallets_reloads_the_screen_you_are_on() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.features.nfts = true;
    let account = |address: &str| wallet_core::session::AccountBalance {
        address: address.into(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::ZERO,
        locked: U256::ZERO,
        nonce: 0,
    };
    // On Collected, with NFTs already loaded for this wallet.
    app.dash.accounts = vec![account("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a")];
    app.switch(Screen::Collected);
    app.eco.nfts = Some(Ok(Vec::new()));
    app.eco.nfts_loading = false;

    // A switch to another wallet: the cached view goes, and so do the accounts until the new
    // wallet's dashboard lands.
    app.eco = super::super::eco::Eco::default();
    app.dash.accounts.clear();
    app.reload_view_on_accounts = true;
    assert!(app.eco.nfts.is_none());

    // The dashboard for the new wallet arrives with its accounts.
    let mut dash = app.dash.clone();
    dash.accounts = vec![account("0x0011223344556677889900112233445566778899")];
    app.on_event(super::super::worker::Ev::Dashboard(Box::new(dash)), (120, 40));

    assert!(!app.reload_view_on_accounts, "the flag was not consumed");
    assert!(app.eco.nfts_loading, "the open screen never asked for the new wallet's NFTs");

    // And it does not fire again on every later dashboard.
    app.eco.nfts_loading = false;
    let mut again = app.dash.clone();
    again.accounts = vec![account("0x0011223344556677889900112233445566778899")];
    app.on_event(super::super::worker::Ev::Dashboard(Box::new(again)), (120, 40));
    assert!(!app.eco.nfts_loading, "a later refresh re-requested a view that was already loaded");
}

/// A quote fills in the tolerance the card cannot guess. It starts at zero — which
/// `ConversionSlippage` rejects — and the right value depends on the size, so the card adopts the
/// quote's suggestion until the user picks one of their own.
#[test]
fn a_conversion_quote_sets_the_slippage_the_card_could_not_guess() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.screen = Screen::Convert;
    app.eco.convert.amount = "250".into();
    assert_eq!(app.eco.convert.slippage_bps, 0, "nothing chosen and nothing quoted yet");
    let quote = |bps: u16| {
        let mut q = wallet_core::ops::ConversionQuote {
            direction: "quai_to_qi".into(),
            amount: "250000000000000000000".into(),
            amount_display: "250 QUAI".into(),
            quoted: None,
            quoted_display: None,
            expected: None,
            expected_display: None,
            implied_slippage_bps: Some(296),
            discount_saturated: false,
            hold: None,
            headline: "250 QUAI → about 1.939 Qi".into(),
            flow_amount: None,
            scenarios: vec![],
            suggested_slippage_bps: bps,
            minimum: None,
            notes: vec![],
            explorer_steps: None,
        };
        q.scenarios.clear();
        Box::new(q)
    };
    app.on_event(Ev::Quote(quote(1310)), (120, 40));
    assert_eq!(app.eco.convert.slippage_bps, 1310, "the suggestion is adopted, not the old fixed 3%");
    // Automatic advice follows the amount/rate until the user explicitly edits it.
    app.on_event(Ev::Quote(quote(70)), (120, 40));
    assert_eq!(app.eco.convert.slippage_bps, 70);
    app.eco.convert.manual_slippage = true;
    app.eco.convert.slippage_bps = 1310;
    app.on_event(Ev::Quote(quote(80)), (120, 40));
    assert_eq!(app.eco.convert.slippage_bps, 1310, "the user's standing choice survives a requote");
}

/// The wrap used to size modals and style prose line by line.
#[test]
fn textwrap_breaks_on_spaces_and_never_returns_nothing() {
    use super::super::ui::textwrap;
    assert_eq!(textwrap("one two three", 7), vec!["one two", "three"]);
    assert_eq!(textwrap("", 10), vec![""], "an empty string still occupies one line");
    assert_eq!(textwrap("supercalifragilistic", 5), vec!["supercalifragilistic"], "a word longer than the width is not cut");
    assert!(textwrap(&"word ".repeat(40), 20).iter().all(|l| l.chars().count() <= 20));
}

/// The Convert screen draws both numbers a tolerance has to clear, and at the floor it drops the
/// scenario list — four rows of "90.00%" — for the one sentence that names the way out.
#[test]
fn the_convert_screen_shows_what_the_conversion_costs_now() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let draw = |app: &mut App| {
        let mut term = Terminal::new(TestBackend::new(180, 44)).unwrap();
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    let quote = |implied: u16, saturated: bool| wallet_core::ops::ConversionQuote {
        direction: "quai_to_qi".into(),
        amount: "250".into(),
        amount_display: "250 QUAI".into(),
        quoted: None,
        quoted_display: Some("1.998 Qi".into()),
        expected: None,
        expected_display: Some("1.939 Qi".into()),
        implied_slippage_bps: Some(implied),
        discount_saturated: saturated,
        hold: None,
        headline: "250 QUAI → about 1.939 Qi".into(),
        flow_amount: None,
        scenarios: vec![wallet_core::ops::RiskScenario {
            label: "you + one similar conversion".into(),
            batch_quai: "500".into(),
            discount_bps: 1260,
        }],
        suggested_slippage_bps: 1310,
        minimum: None,
        notes: vec![],
        explorer_steps: None,
    };
    app.switch(Screen::Convert);
    app.eco.convert.amount = "250".into();
    app.eco.convert.quote = Some(quote(296, false));
    let normal = draw(&mut app);
    assert!(normal.contains("what it costs right now"), "the observation is on the screen");
    assert!(normal.contains("2.96%"), "and as a percentage, not basis points");
    assert!(normal.contains("refund risk if others share the block") && normal.contains("12.60%"), "the model is there too");
    // The card had no tolerance of its own, so it shows the quote's rather than a fixed 3%.
    assert!(normal.contains("13.10%"), "the suggested tolerance is what the card offers");
    // At the floor the bars go and the explanation arrives.
    app.eco.convert.quote = Some(quote(9000, true));
    let floored = draw(&mut app);
    assert!(floored.contains("discount is at its floor"), "the floor is named");
    assert!(floored.contains("market route"), "with the way out");
    assert!(!floored.contains("refund risk if others share the block"), "and without four rows of 90%");
}

/// The marketplace does a handful of sales a week, so a fixed seven-day window left almost every
/// Explore row reading "—" with good history behind it. The window widens until it holds a sale,
/// is chosen once market-wide so the rows stay comparable, and the labels say which one it is.
#[test]
fn the_trade_window_widens_until_it_holds_a_sale() {
    use super::super::eco::TRADE_WINDOWS;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let now = wallet_core::registry::now();
    let sale = |days_ago: u64, price: f64| wallet_core::market::Trade {
        tx_hash: String::new(),
        contract: "0x00aa".into(),
        token_id: "1".into(),
        seller: String::new(),
        buyer: String::new(),
        price_quai: Some(price),
        currency: "0x0000000000000000000000000000000000000000".into(),
        kind: "ask_filled".into(),
        at: now.saturating_sub(days_ago * 86_400),
        name: None,
        image: None,
    };
    assert_eq!(TRADE_WINDOWS, [7, 30, 90, 365]);
    // Nothing at all: the longest window, so the label never claims a week it cannot back.
    assert_eq!(app.eco.trade_window_days(), 365, "no sales falls back to the longest");
    // A sale inside the week keeps the week.
    app.eco.nft_trades = vec![sale(3, 50.0)];
    assert_eq!(app.eco.trade_window_days(), 7);
    // The real shape on 2026-09-21: nothing in seven days, three in thirty.
    app.eco.nft_trades = vec![sale(10, 50.0), sale(12, 350.0), sale(29, 50.0)];
    assert_eq!(app.eco.trade_window_days(), 30, "widened past the empty week");
    let (volume, sales) = app.eco.nft_window("0x00aa", app.eco.trade_window_days());
    assert_eq!((volume, sales), (450.0, 3), "and the widened window is the one the rows count in");
    // Older still: 90 days.
    app.eco.nft_trades = vec![sale(60, 5.0)];
    assert_eq!(app.eco.trade_window_days(), 90);
}

/// Explore carries the marketplace tape beside the directory: what is actually changing hands,
/// newest first, with this wallet's own side of a sale marked. It only appears where there is room
/// for it — a squeezed tape costs the collection names more than it adds.
#[test]
fn explore_shows_recent_buys_beside_the_directory() {
    use ratatui::{Terminal, backend::TestBackend};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let now = wallet_core::registry::now();
    let sale = |name: &str, price: f64, buyer: &str, days: u64| wallet_core::market::Trade {
        tx_hash: String::new(),
        contract: "0x00aa".into(),
        token_id: "70".into(),
        seller: "0x00bb".into(),
        buyer: buyer.into(),
        price_quai: Some(price),
        currency: "0x0000000000000000000000000000000000000000".into(),
        kind: "ask_filled".into(),
        at: now.saturating_sub(days * 86_400),
        name: Some(name.into()),
        image: None,
    };
    let draw = |app: &mut App, w: u16| {
        let mut term = Terminal::new(TestBackend::new(w, 40)).unwrap();
        term.draw(|f| super::super::ui::draw(f, app)).unwrap();
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect::<String>()
    };
    app.switch(Screen::Explore);
    // Nothing traded: no tape, and the directory keeps the whole width.
    assert!(!draw(&mut app, 160).contains("recent buys"), "an empty tape is not worth the columns");
    app.eco.nft_trades = vec![sale("ELEPHANT", 50.0, "0x00cc", 4), sale("SQUID", 350.0, "0x00dd", 9)];
    let wide = draw(&mut app, 160);
    assert!(wide.contains("recent buys · 2"), "the tape is there with its count");
    assert!(wide.contains("ELEPHANT") && wide.contains("350 QUAI"), "newest first, with prices: {wide:.0}");
    assert!(wide.contains("over 1 sale ") && !wide.contains("1 sales"), "one sale is not plural: {}", &wide[..0]);
    // Too narrow: the directory wins the space.
    assert!(!draw(&mut app, 100).contains("recent buys"), "no tape at 100 columns");
}

/// What the Markets screen refreshes fast must not sit behind somebody else's slow cache.
///
/// A tick landing inside a source's TTL is served from the store and never reaches the network, so
/// the *slower* of the two is what actually moves on screen — and nothing errors when that goes
/// wrong, the numbers simply stop changing. These are all compile-time constants, so a bad edit
/// should fail the build rather than wait for anyone to run the tests.
#[test]
fn the_fast_market_reads_do_not_sit_behind_a_slow_cache() {
    use crate::tui::eco::MARKET_REFRESH;
    use wallet_core::launches::{LAUNCH_TTL, TRADES_TTL};
    use wallet_core::markets::{DIRECTORY_TTL, FACTORY_TTL};

    // Price and TVL come off reserves read from the node, which nothing else caches, so the tick
    // is the only thing pacing them. The directory is deliberately slower — it answers which pools
    // exist, not what is in them — and if it were the faster of the two the reserve read would be
    // pointless work.
    const { assert!(MARKET_REFRESH.as_secs() < DIRECTORY_TTL, "reserves exist to beat the directory's pace") };
    // 30 s is the explorer's own `max-age` on the pool page. Asking more often cannot return
    // anything newer; it only spends a budget the rest of the screen shares.
    const { assert!(DIRECTORY_TTL >= 30, "the explorer serves this page max-age=30") };

    // The tape mixes curve trades from the launch index with pool swaps read from the chain. If
    // the index were cached for longer, curve rows would lag the pool rows beside them.
    const { assert!(TRADES_TTL <= MARKET_REFRESH.as_secs(), "curve rows must keep the tape's pace") };

    // The Launches list is ordered by stage, so a stale page is a stale ordering, not just stale
    // numbers — and a token that launched in the meantime is simply missing.
    const { assert!(LAUNCH_TTL <= 30, "a launch list this old puts the rows in the wrong order") };
    // Stages the index leaves null are filled in from the launchpad itself, so that read must not
    // be the slower of the two: the list would order by a stage older than the list carrying it.
    const { assert!(FACTORY_TTL <= LAUNCH_TTL, "the launchpad read fills in the stages this list sorts on") };
    // A launch-AMM or QuaiSwap pair is discovered over RPC rather than from the explorer. It may
    // lag — the set of pairs changes when someone deploys one — but not so far that a row looks
    // abandoned, and its reserves are refreshed on the fast path regardless.
    const { assert!(FACTORY_TTL <= 60, "a launch-AMM row more than a minute old reads as a dead market") };
}

/// The Launches list is the curves still raising, nearest graduation first — and a token that has
/// finished and can be bought on an exchange belongs to Markets, not here.
#[test]
fn launches_lead_with_the_curve_nearest_graduation_and_drop_what_markets_already_carries() {
    use wallet_core::launches::{Launch, Phase};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let launch = |token: &str, symbol: &str, phase, bps: Option<u64>| Launch {
        token: token.into(),
        symbol: symbol.into(),
        phase,
        progress_bps: bps,
        ..Default::default()
    };
    app.eco.launches = Some(Ok(vec![
        // Past its curve, and `with_pools` lists a SMOL pair: Markets has it, so it goes.
        launch("0x00a1", "SMOL", Phase::Pooled, Some(10_000)),
        launch("0x00c1", "EARLY", Phase::Bonding, Some(1_200)),
        // Past its curve with no pool anywhere: it stays, or its claim key becomes unreachable.
        launch("0x00c2", "ORPHAN", Phase::Graduated, Some(10_000)),
        launch("0x00c3", "NEARLY", Phase::Bonding, Some(9_400)),
        // A launchpad that would not say how far along it is sorts below the ones that did.
        launch("0x00c4", "UNKNOWN", Phase::Bonding, None),
    ]));
    let shown: Vec<String> = app.launch_rows().iter().map(|l| l.symbol.clone()).collect();
    assert_eq!(shown, vec!["NEARLY", "EARLY", "UNKNOWN", "ORPHAN"], "stage order, and SMOL is a market now");
    // A graduated token reads as 100%: it must not outrank a curve still raising, or the screen
    // opens on a launch that is already over.
    assert_eq!(shown.first().map(String::as_str), Some("NEARLY"));

    // Nothing is hidden on the strength of a directory that has not loaded: without it, the rows
    // Markets would have claimed are still listed rather than silently dropped.
    app.eco.markets_view.pools = None;
    let unloaded: Vec<String> = app.launch_rows().iter().map(|l| l.symbol.clone()).collect();
    assert!(unloaded.contains(&"SMOL".to_string()), "no directory, no filtering: {unloaded:?}");
}

/// A curve trade reads like every other tape row, though no pair in the directory matches it.
///
/// The tape asks the directory which side of a swap is the base, and a bonding curve has no pair
/// there — the trade happened on the curve contract. Without a fallback every curve row drew with
/// no price and no direction, in plain text among coloured pool rows.
#[test]
fn a_curve_trade_knows_which_side_it_is_about_without_a_pool() {
    use wallet_core::markets::{DexSwap, PoolToken};
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    let wquai = wallet_core::sdk::wrappers::WQUAI_MAINNET_ADDRESS.to_lowercase();
    let tok = |a: &str, s: &str| PoolToken { address: a.into(), symbol: s.into(), decimals: 18 };
    let buy = DexSwap {
        at: 1_790_026_405,
        timed: true,
        block: 10_220_299,
        tx: "0x004c".into(),
        index: 1,
        // The curve contract, which is in no pool directory.
        pool: "0x004bc407903a51506bcf0b1ab423958c5991c237".into(),
        token_in: tok(&wquai, "QUAI"),
        token_out: tok("0x0035187a", "QAXE"),
        amount_in: 4_200.0,
        amount_out: 1_015_403.5,
        trader: "0x0051".into(),
    };
    let base = app.flow_base(&buy, None).expect("the launch token is the base");
    assert_eq!(base.symbol, "QAXE", "the side that is not money is what the row is about");
    assert!(buy.buys(&base.address), "a curve buy reads as a buy");
    assert!(buy.price(&base.address).is_some_and(|p| p > 0.0), "and it has a price to show");

    // Read backwards, the same trade is a sell of the same base.
    let sell = DexSwap { token_in: buy.token_out.clone(), token_out: buy.token_in.clone(), ..buy.clone() };
    let base = app.flow_base(&sell, None).expect("still the launch token");
    assert_eq!(base.symbol, "QAXE");
    assert!(!sell.buys(&base.address), "a curve sell reads as a sell");

    // Money both sides: nothing says which one the row is about, and it draws plainly rather than
    // guessing a direction.
    let usdt = app.config.network("mainnet").unwrap().ecosystem.usdt.unwrap().address;
    let both = DexSwap { token_out: tok(&usdt, "USDT"), ..buy.clone() };
    assert!(app.flow_base(&both, None).is_none(), "QUAI for USDT has no launch side");

    // A pair the directory does carry still decides it the way the pairs list does.
    let pools = app.eco.markets_view.pools.clone().unwrap().unwrap().0;
    let pair = pools.iter().find(|p| p.token0.symbol == "SMOL").expect("a SMOL pair");
    assert!(app.flow_base(&buy, Some(pair)).is_some(), "a known pool is unaffected by the fallback");
}

// Maintained regressions from the review of commit 4aa780f.
// These assert desired behavior through the same event paths as the application.
// No keys, RPC calls, or transactions are involved.

fn review_probe_flow(app: &mut App) {
    use super::super::eco::FlowKind;
    app.dash.unlocked = true;
    app.start_flow(FlowKind::Swap {
        account: None,
        from: "token-in".into(),
        to: "token-out".into(),
        amount: "1".into(),
        slippage: 50,
        deadline: 10,
        label: "review probe".into(),
        prewrap: None,
        unwrap_after: false,
        baseline: "0".into(),
        then: None,
    });
}

#[test]
fn review_probe_commit_failure_must_release_or_recover_flow() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    review_probe_flow(&mut app);
    app.on_event(review("probe", "swap"), (100, 30));
    // Approve has closed the modal; commit now fails (e.g. its snapshot expired).
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    app.on_event(
        Ev::CommitError { op_id: "probe".into(), message: "snapshot expired while committing".into(), ambiguous: false },
        (100, 30),
    );
    app.advance_flow();
    assert!(
        app.eco.flow.as_ref().is_none_or(|f| f.review_op.is_none()),
        "flow still waits for a review that is no longer open; all new flows are blocked"
    );
}

#[test]
fn review_probe_lock_during_commit_must_keep_submission() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    review_probe_flow(&mut app);
    app.on_event(review("probe", "swap"), (100, 30));
    app.modal = Modal::None;
    app.committing_kind = Some("swap".into());
    // Lock occurs while an already-approved commit is finishing.
    app.enter_lock(None);
    app.on_event(submitted("probe"), (100, 30));
    app.dash.ops = vec![op("probe", "swap", wallet_core::appdb::OpStatus::Confirmed)];
    app.locked = false;
    app.dash.unlocked = true;
    app.advance_flow();
    assert!(app.eco.flow.is_none(), "the completed swap is requested again after unlocking");
}

#[test]
fn review_probe_typing_conversion_must_not_choose_slippage() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.screen = Screen::Convert;
    app.eco.convert.field = 1;
    assert_eq!(app.eco.convert.slippage_bps, 0);
    press(&mut app, KeyCode::Char('2'));
    assert_eq!(app.eco.convert.amount, "2");
    assert_eq!(app.eco.convert.slippage_bps, 0, "typing chooses 300 bps before any quote can suggest a tolerance");
}

#[test]
fn review_probe_amount_edit_must_invalidate_swap_quote_immediately() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.screen = Screen::Swap;
    app.eco.swap.field = 1;
    app.eco.swap.amount = "1".into();
    app.eco.swap.quote_key = 42;
    app.eco.swap.requested_key = 42;
    app.eco.swap.to = Some(wallet_core::swap::SwapAsset::Quai);
    app.eco.swap.requested_input = app.swap_input_key();
    app.eco.swap.quoted_at = Some(Instant::now());
    assert!(app.swap_quote_current());
    press(&mut app, KeyCode::Char('0'));
    assert_eq!(app.eco.swap.amount, "10");
    assert!(!app.swap_quote_current(), "the previous amount's quote remains current during debounce");
}

#[test]
fn a_trade_checkpoint_survives_restart_and_requires_explicit_resume() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    review_probe_flow(&mut app);
    let plan = app.eco.flow.as_ref().unwrap().checkpoint.as_ref().unwrap().clone();
    assert_eq!(plan.revision, 2);
    assert!(
        wallet_core::plans::claim(&app.paths.wallet_dir(&plan.owner).join("plan-locks"), &plan.id).is_err(),
        "a second client cannot own the plan"
    );
    let (paths, config, theme, caps, meta) = (app.paths.clone(), app.config.clone(), app.theme.clone(), app.caps.clone(), app.meta.clone());
    drop(app);
    let mut restored = App::new(paths, "local".into(), config, theme, caps, meta);
    assert!(restored.eco.flow.is_none(), "opening does not authorize resumption");
    restored.locked = false;
    restored.dash.unlocked = true;
    restored.resume_trade_plan();
    let flow = restored.eco.flow.as_ref().expect("explicit resume restores intent");
    assert_eq!(flow.checkpoint.as_ref().unwrap().id, plan.id);
    assert!(flow.requested, "remaining action still needs a fresh review");
    assert!(flow.review_op.is_none());
}

#[test]
fn swap_identity_includes_deadline_owner_network_and_age() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.eco.swap.amount = "1".into();
    app.eco.swap.to = Some(wallet_core::swap::SwapAsset::Quai);
    app.eco.swap.requested_key = 9;
    app.eco.swap.quote_key = 9;
    app.eco.swap.requested_input = app.swap_input_key();
    app.eco.swap.quoted_at = Some(Instant::now());
    assert!(app.swap_quote_current());
    app.eco.swap.amount = "1.0".into();
    assert!(app.swap_quote_current(), "normalization preserves equivalent inputs");
    app.eco.swap.deadline_minutes += 1;
    assert!(!app.swap_quote_current());
    app.eco.swap.requested_input = app.swap_input_key();
    app.network_id = "other".into();
    assert!(!app.swap_quote_current());
    app.eco.swap.requested_input = app.swap_input_key();
    app.eco.swap.quoted_at = Some(Instant::now() - std::time::Duration::from_secs(21));
    assert!(!app.swap_quote_current());
}

#[test]
fn order_review_stays_separate_from_unrelated_flows_and_late_observations() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let Ev::Review(r) = review("order-op", "swap") else { unreachable!() };
    app.on_event(Ev::OrderReview(r.clone()), (100, 30));
    assert!(matches!(&app.modal,Modal::Review(state) if state.review.op_id=="order-op"));
    let wallet = app.meta.as_ref().unwrap().id.clone();
    let network = app.dash.network_id.clone();
    app.on_event(Ev::Orders { wallet, network, rows: vec![] }, (100, 30));
    assert!(matches!(&app.modal,Modal::Review(state) if state.review.op_id=="order-op"), "late observation must not hide approval");
    app.modal = Modal::None;
    review_probe_flow(&mut app);
    app.on_event(Ev::OrderReview(r), (100, 30));
    assert!(!matches!(app.modal, Modal::Review(_)), "order cannot attach to unrelated flow");
    assert!(app.eco.flow.as_ref().unwrap().review_op.is_none());
}

#[test]
fn trading_modal_defaults_honor_configuration_and_conversion_remains_automatic() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.swap_slippage_bps = 123;
    app.config.swap_deadline_minutes = 42;
    for kind in [
        FormKind::CurveBuy { token: "t".into(), symbol: "T".into(), curve: "c".into() },
        FormKind::CurveSell { token: "t".into(), symbol: "T".into(), curve: "c".into(), held: "1".into() },
        FormKind::RemoveLiquidity { pair: "p".into(), name: "pool".into() },
    ] {
        app.open_form(kind);
        let Modal::Form(form) = &app.modal else { panic!("form") };
        assert_eq!(form.fields.iter().find(|f| f.label == "Slippage").unwrap().value, "123");
    }
    for kind in [FormKind::ConvertQuaiToQi, FormKind::ConvertQiToQuai] {
        app.open_form(kind);
        let Modal::Form(form) = &app.modal else { panic!("form") };
        let tolerance = form.fields.iter().find(|f| f.label == "Slippage").unwrap();
        assert!(tolerance.optional && tolerance.value.is_empty(), "no implicit manual limit");
    }
}

#[test]
fn partial_stake_form_preserves_selected_owner_gauge_and_amount() {
    use super::super::eco::FlowKind;
    for stake in [true, false] {
        let (_dir, mut app) = test_app(WalletKind::Hd);
        app.dash.unlocked = true;
        app.open_form(FormKind::StakePosition {
            pair: "pair".into(),
            gauge: Some("gauge".into()),
            name: "LP".into(),
            amount: "100".into(),
            stake,
        });
        let Modal::Form(mut form) = std::mem::replace(&mut app.modal, Modal::None) else { panic!("form") };
        form.fields[0].value = "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into();
        form.fields[1].value = "12.5".into();
        app.submit_form(&form);
        let FlowKind::Steps { prepare, .. } = &app.eco.flow.as_ref().unwrap().kind else { panic!("steps") };
        match prepare.as_ref() {
            Prepare::StakeNext { account, pair, gauge, amount } if stake => {
                assert_eq!(account.as_deref(), Some(form.fields[0].value.as_str()));
                assert_eq!(pair, "pair");
                assert_eq!(gauge.as_deref(), Some("gauge"));
                assert_eq!(amount, "12.5");
            }
            Prepare::Unstake { account, pair, gauge, amount } if !stake => {
                assert_eq!(account.as_deref(), Some(form.fields[0].value.as_str()));
                assert_eq!(pair, "pair");
                assert_eq!(gauge.as_deref(), Some("gauge"));
                assert_eq!(amount, "12.5");
            }
            _ => panic!("wrong operation"),
        }
    }
}

#[test]
fn bounded_swap_form_does_not_drop_or_default_explicit_limits() {
    use super::super::eco::FlowKind;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.unlocked = true;
    app.open_form(FormKind::BoundedSwap { from: "quai".into(), to: "token".into(), input: "1".into() });
    let Modal::Form(mut form) = std::mem::replace(&mut app.modal, Modal::None) else { panic!("form") };
    form.fields[0].value = "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into();
    assert!(validate(&form).is_err(), "must choose an explicit bound");
    form.fields[2].value = "25.5".into();
    form.fields[3].value = "99.5".into();
    assert!(validate(&form).is_err(), "fractional basis points cannot fall back to no bound");
    form.fields[3].value = "99".into();
    assert!(validate(&form).is_ok());
    app.submit_form(&form);
    let FlowKind::Steps { prepare, .. } = &app.eco.flow.as_ref().unwrap().kind else { panic!("steps") };
    let Prepare::Trading { intent } = prepare.as_ref() else { panic!("intent") };
    let wallet_core::execution::TradingAction::BoundedSwap { bounds, .. } = &intent.action else { panic!("bound action") };
    assert_eq!(bounds.minimum_output.as_deref(), Some("25.5"));
    assert_eq!(bounds.maximum_impact_bps, Some(99));
}

fn market_conversion_app(direction: wallet_core::qi_market::Direction) -> (tempfile::TempDir, App, String, String) {
    let (dir, mut app) = test_app(WalletKind::Hd);
    let network = wallet_core::network::NetworkProfile::builtins().remove(0);
    app.network_id = network.id.clone();
    app.dash.network_id = network.id;
    // `test_app` builds its fixture with `create_watch`, which fills `watch` and leaves
    // `quai_accounts` empty, so the signing account comes from the same constant the rest of
    // these tests use rather than from metadata.
    let owner = "0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".to_string();
    app.dash.accounts = vec![wallet_core::session::AccountBalance {
        address: owner.clone(),
        label: "Main".into(),
        hd_index: Some(0),
        balance: U256::from(100000000000000000000u128),
        locked: U256::ZERO,
        nonce: 0,
    }];
    app.dash.unlocked = true;
    let wqi = network.wqi.unwrap();
    app.start_qi_route(direction, "2".into(), 50);
    (dir, app, owner, wqi)
}

#[test]
fn market_conversion_checkpoints_receipt_and_residual_while_locked_without_preparing() {
    use super::super::eco::FlowKind;
    use wallet_core::qi_market::Direction;
    for paid in ["500000000000000000", "2500000000000000000"] {
        let (_dir, mut app, owner, wqi) = market_conversion_app(Direction::QuaiToQi);
        let mut operation = op("market-swap", "swap", wallet_core::appdb::OpStatus::Confirmed);
        operation.account = owner;
        operation.detail = serde_json::json!({"to_token":wqi,"actual_out":paid});
        app.dash.ops = vec![operation];
        let flow = app.eco.flow.as_mut().unwrap();
        flow.requested = false;
        flow.waiting = Some("market-swap".into());
        flow.last_operation = Some("market-swap".into());
        let plan_id = flow.checkpoint.as_ref().unwrap().id.clone();
        app.locked = true;
        app.toasts.clear();
        app.advance_flow();
        assert!(app.toasts.is_empty(), "locked checkpoint processing must not expose trade amounts");
        if paid.starts_with('5') {
            assert!(app.eco.flow.is_none(), "sub-one-Qi residual completes rather than waiting forever");
        } else {
            let flow = app.eco.flow.as_ref().unwrap();
            assert!(!flow.requested && flow.waiting.is_none());
            let FlowKind::Steps { prepare, .. } = &flow.kind else { panic!("core steps") };
            let Prepare::Trading { intent } = prepare.as_ref() else { panic!("intent") };
            assert!(
                matches!(&intent.action,wallet_core::execution::TradingAction::MarketConversion {stage:1,amount,residual_atoms,..} if amount=="2" && residual_atoms=="500000000000000000")
            );
            assert_eq!(flow.checkpoint.as_ref().unwrap().id, plan_id);
            assert!(flow.checkpoint.as_ref().unwrap().revision > 1, "advanced intent was saved before any next review");
        }
    }
}

#[test]
fn market_conversion_requires_wrap_settlement_before_claim_review() {
    use super::super::eco::FlowKind;
    use wallet_core::qi_market::Direction;
    let (_dir, mut app, owner, _) = market_conversion_app(Direction::QiToQuai);
    let mut operation = op("deposit", "wrap_qi", wallet_core::appdb::OpStatus::Confirmed);
    operation.amount = wallet_core::amount::parse_qi("2").unwrap().to_string();
    operation.detail = serde_json::json!({"beneficiary":owner});
    app.dash.ops = vec![operation];
    let flow = app.eco.flow.as_mut().unwrap();
    flow.requested = false;
    flow.waiting = Some("deposit".into());
    flow.last_operation = Some("deposit".into());
    app.advance_flow();
    assert!(!app.eco.flow.as_ref().unwrap().requested, "inclusion alone cannot claim a locked deposit");
    app.dash.ops[0].status = wallet_core::appdb::OpStatus::Settled;
    app.advance_flow();
    let flow = app.eco.flow.as_ref().unwrap();
    assert!(flow.requested);
    let FlowKind::Steps { prepare, .. } = &flow.kind else { panic!("steps") };
    let Prepare::Trading { intent } = prepare.as_ref() else { panic!("intent") };
    assert!(matches!(intent.action, wallet_core::execution::TradingAction::MarketConversion { stage: 1, .. }));
}

/// `g` then a letter goes straight to a screen; `g g` is the first row; the sheet runs an
/// item by its letter; the palette's key column is the keymap's, not a hand-written path.
#[test]
fn go_to_chords_and_the_action_sheet() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    press(&mut app, KeyCode::Char('g'));
    assert!(matches!(app.modal, Modal::GoTo));
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(app.screen, Screen::Markets, "g m: markets");
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(app.screen, Screen::Activity, "g a: activity");
    app.selected = 3;
    press(&mut app, KeyCode::Char('g'));
    press(&mut app, KeyCode::Char('g'));
    assert_eq!(app.selected, 0, "g g: the first row");
    // A screen with a sheet: the letter runs the item.
    app.switch(Screen::Qi);
    press(&mut app, KeyCode::Char(' '));
    let Modal::Sheet { items, .. } = &app.modal else { panic!("Qi has actions") };
    assert!(items.iter().any(|i| i.key == 'n' && i.label.contains("new address")));
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Modal::None), "esc closes the sheet");
    // The palette's keys come from the tables.
    assert_eq!(super::super::palette::keys_for("scan_qi"), "g q R");
    assert_eq!(super::super::palette::keys_for("aggregate"), "g q space a");
    assert_eq!(super::super::palette::keys_for("lock"), "ctrl-l");
    assert_eq!(super::super::palette::keys_for("launches"), "g l");
}

/// The auto-lock warning is a warning, not an error; it counts down, and a key that keeps the
/// wallet open takes it away at once.
#[test]
fn the_autolock_warning_counts_down_and_goes_on_a_key() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.auto_lock_minutes = 2;
    app.last_input = Instant::now() - std::time::Duration::from_secs(90);
    app.tick((100, 30));
    let warning = app.toasts.iter().find(|t| t.id == Some("autolock")).expect("warned");
    assert_eq!(warning.level, super::Severity::Attention);
    assert!(warning.text.starts_with("locking in 30s") || warning.text.starts_with("locking in 29s"), "{}", warning.text);
    app.last_input = Instant::now() - std::time::Duration::from_secs(100);
    app.tick((100, 30));
    let text = &app.toasts.iter().find(|t| t.id == Some("autolock")).expect("still warning").text;
    assert!(text.starts_with("locking in 20s") || text.starts_with("locking in 19s"), "the countdown counts: {text}");
    app.on_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE), (100, 30));
    app.tick((100, 30));
    assert!(app.toasts.iter().all(|t| t.id != Some("autolock")), "a key keeps it open and the warning goes");
}

/// One exchange: whatever pair is chosen, the view that can carry it takes over. QUAI and Qi
/// convert, Qi and WQI or QUAI and WQUAI wrap, anything else swaps, and a pair with no route is
/// refused with the way that does exist. What was typed follows while the side paid stays.
#[test]
fn the_exchange_routes_every_pair_to_the_view_that_carries_it() {
    use super::super::eco::ExAsset;
    use wallet_core::swap::SwapAsset;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.network_id = "mainnet".into();
    let net = app.net().expect("mainnet profile");
    let token = |address: &Option<String>, symbol: &str| {
        ExAsset::Swap(SwapAsset::Token { address: address.clone().unwrap().to_lowercase(), symbol: symbol.into(), decimals: 18 })
    };
    let (wqi, wquai) = (token(&net.wqi, "WQI"), token(&net.wquai, "WQUAI"));
    let usdt = token(&net.ecosystem.usdt.as_ref().map(|u| u.address.clone()), "USDT");
    app.switch(Screen::Swap);
    app.pick_exchange(false, usdt.clone());
    app.eco.swap.amount = "5".into();
    assert_eq!(app.exchange_pair(), (ExAsset::Swap(SwapAsset::Quai), Some(usdt.clone())));
    // QUAI → Qi: a conversion, the 5 QUAI typed carried over.
    app.pick_exchange(false, ExAsset::Qi);
    assert_eq!(app.screen, Screen::Convert);
    assert!(!app.eco.convert.qi_to_quai);
    assert_eq!(app.eco.convert.amount, "5", "the amount follows while QUAI is still paid");
    // Paying Qi instead turns the pair around.
    app.pick_exchange(true, ExAsset::Qi);
    assert_eq!(app.screen, Screen::Convert);
    assert!(app.eco.convert.qi_to_quai, "Qi → QUAI");
    assert!(app.eco.convert.amount.is_empty(), "an amount typed in QUAI is not an amount of Qi");
    // Qi → WQI wraps; WQI → Qi redeems.
    app.pick_exchange(false, wqi.clone());
    assert_eq!((app.screen, app.eco.wrap.mode), (Screen::Wrap, 0));
    app.pick_exchange(true, wqi.clone());
    assert_eq!((app.screen, app.eco.wrap.mode), (Screen::Wrap, 2), "choosing the other side's asset turns the pair around");
    // QUAI ↔ WQUAI.
    app.pick_exchange(true, ExAsset::Swap(SwapAsset::Quai));
    app.pick_exchange(false, wquai.clone());
    assert_eq!((app.screen, app.eco.wrap.mode), (Screen::Wrap, 3));
    app.pick_exchange(true, wquai.clone());
    assert_eq!((app.screen, app.eco.wrap.mode), (Screen::Wrap, 4));
    // Qi with a token it has no route to: refused, and the pair stays as it was.
    app.pick_exchange(true, ExAsset::Qi);
    app.toasts.clear();
    app.pick_exchange(false, usdt.clone());
    assert!(app.toasts.iter().any(|t| t.text.contains("Qi trades with QUAI")), "{:?}", app.toasts);
    // Anything else swaps.
    app.pick_exchange(true, wquai.clone());
    app.pick_exchange(false, usdt.clone());
    assert_eq!(app.screen, Screen::Swap);
    assert_eq!(app.exchange_pair(), (wquai, Some(usdt)));
    // Choosing Qi always works: opposite a token it cannot pair with, the other side becomes
    // QUAI, and the app says so.
    app.toasts.clear();
    app.pick_exchange(true, ExAsset::Qi);
    assert_eq!(app.screen, Screen::Convert);
    assert!(app.eco.convert.qi_to_quai);
    assert!(app.toasts.iter().any(|t| t.text.contains("USDT became QUAI")), "{:?}", app.toasts);
    // The Exchange tab keeps whichever of them was last open.
    assert_eq!(app.last_exchange, Screen::Convert);
}

/// Settings step both ways with ← and →, and a watch-only wallet is not offered what it has no
/// keys for.
#[test]
fn settings_step_both_ways_and_fit_the_wallet() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.switch(Screen::Settings);
    app.selected = app.settings_rows().iter().position(|(id, _)| *id == "motion").unwrap();
    let start = app.config.motion;
    press(&mut app, KeyCode::Right);
    assert_ne!(app.config.motion, start);
    press(&mut app, KeyCode::Left);
    assert_eq!(app.config.motion, start, "← undoes →");
    press(&mut app, KeyCode::Left);
    assert_eq!(app.config.motion, Motion::Off, "and steps back past the start, round the list");
    assert!(app.settings_rows().iter().any(|(id, _)| *id == "phrase"));
    let (_dir2, watch) = test_app(WalletKind::Watch);
    let rows: Vec<&str> = watch.settings_rows().iter().map(|(id, _)| *id).collect();
    for hidden in ["phrase", "backup", "autolock", "daemon_unlock"] {
        assert!(!rows.contains(&hidden), "a watch-only wallet is not offered {hidden}");
    }
}

/// `W` opens the wallet switcher on the open wallet; enter on another switches (the open one
/// locks), enter on the open one only closes, and m goes to the Wallets screen to manage them.
#[test]
fn the_wallet_switcher_opens_everywhere_and_switches() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let other = app.registry.create_watch("second", &[("0x002360Bc8E2A359bE7335B06De43F1c7F040f15a".into(), "Main".into())]).unwrap();
    app.switch(Screen::Markets);
    press(&mut app, KeyCode::Char('W'));
    let Modal::Wallets { selected } = app.modal else { panic!("W opens the switcher") };
    assert_eq!(app.wallets[selected].id, app.meta.as_ref().unwrap().id, "on the open wallet");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(app.modal, Modal::None) && app.meta.as_ref().unwrap().id != other.id, "enter on the open one only closes");
    press(&mut app, KeyCode::Char('W'));
    let target = app.wallets.iter().position(|w| w.id == other.id).unwrap();
    app.modal = Modal::Wallets { selected: target };
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.meta.as_ref().unwrap().id, other.id, "enter on another switches to it");
    press(&mut app, KeyCode::Char('W'));
    press(&mut app, KeyCode::Char('m'));
    assert_eq!(app.screen, Screen::Wallets, "m manages");
}

/// A screen is left where it was: coming back finds the cursor on the same row, even when the
/// list reordered meanwhile; Backspace and ctrl-o walk back through the screens visited.
#[test]
fn screens_remember_where_they_were_left_and_backspace_returns() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    with_pools(&mut app);
    app.switch(Screen::Markets);
    app.selected = 2;
    let pair = app.market_rows()[2].address.clone();
    app.switch(Screen::Activity);
    app.switch(Screen::Settings);
    // The pairs reorder while away: watching the pair moves it to the top.
    app.eco.watchlist = vec![pair.clone()];
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.screen, Screen::Activity, "backspace: the screen before");
    app.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL), (100, 30));
    assert_eq!(app.screen, Screen::Markets, "ctrl-o too");
    assert_eq!(app.market_rows()[app.selected].address, pair, "the same pair under the cursor, wherever it moved");
    press(&mut app, KeyCode::Backspace);
    assert_eq!(app.screen, Screen::Home, "and on back to where it started");
}

/// A sequence says where it stands: what was sent, what is under way, and what is known to
/// follow — an approval the worker asked for appears as the step under review, never guessed.
#[test]
fn a_flow_steps_through_what_it_has_sent_and_what_follows() {
    use super::super::eco::{Flow, FlowKind, NextSwap, StepState::*};
    let flow = |done: &[&str], swapped: bool| Flow {
        checkpoint: None,
        lease: None,
        kind: FlowKind::Swap {
            account: None,
            from: "quai".into(),
            to: "0x00aa".into(),
            amount: "1".into(),
            slippage: 50,
            deadline: 10,
            label: "swap".into(),
            prewrap: None,
            unwrap_after: true,
            baseline: "0".into(),
            then: Some(NextSwap { to: "quai".into(), unwrap_after: true, hub_decimals: 18, first: None, polls: 0 }),
        },
        swapped,
        requested: false,
        review_op: None,
        waiting: None,
        last_operation: None,
        steps: 0,
        done: done.iter().map(|s| s.to_string()).collect(),
        last_poll: Instant::now(),
    };
    let names = |v: Vec<(String, super::super::eco::StepState)>| v;
    assert_eq!(names(flow(&[], false).stepper(None)), vec![("swap".into(), Now), ("swap on".into(), Next), ("unwrap WQUAI".into(), Next)],);
    // An approval under review sits before the swap it unlocks.
    assert_eq!(
        names(flow(&[], false).stepper(Some("approve"))),
        vec![("approve".into(), Now), ("swap".into(), Next), ("swap on".into(), Next), ("unwrap WQUAI".into(), Next)],
    );
    assert_eq!(
        names(flow(&["approve", "swap"], true).stepper(None)),
        vec![("approve".into(), Done), ("swap".into(), Done), ("swap on".into(), Now), ("unwrap WQUAI".into(), Next)],
    );
}

/// The window title says what the wallet is doing and never what it holds or what it is called:
/// titles show in task bars, window switchers and screen shares.
#[test]
fn the_window_title_says_what_is_happening_and_nothing_it_holds() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.dash.notifications.clear();
    assert_eq!(app.window_title(), "Quai Terminal");
    app.locked = true;
    assert!(app.window_title().ends_with("locked"), "{}", app.window_title());
    assert_eq!(app.taskbar_state(), 0, "nothing shown busy while locked");
    app.locked = false;
    let mut op = op("o1", "send", wallet_core::appdb::OpStatus::Submitted);
    op.amount = "123456".into();
    app.dash.ops.push(op);
    let title = app.window_title();
    assert!(title.ends_with("1 confirming"), "{title}");
    assert_eq!(app.taskbar_state(), 3, "busy while it confirms");
    let name = app.meta.as_ref().unwrap().name.clone();
    assert!(!title.contains(&name) && !title.contains('$') && !title.contains("123456") && !title.contains("QUAI "), "{title}");
}

/// Money arriving is said by name, marked on the rail until Activity is opened (at every motion
/// level), and, with effects on, runs in along the header, lights the hero's gutter and its row.
/// The worker's own "Incoming payment" notice is not said a second time.
#[test]
fn an_arrival_names_its_sender_and_marks_activity_until_seen() {
    use super::super::edge::Signal;
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.motion = wallet_core::config::Motion::Full;
    let mut dash = app.dash.clone();
    dash.network_id = "local".into();
    dash.refreshed_at = 1;
    app.dash = dash.clone();
    let mut next = dash.clone();
    next.activity.push(wallet_core::appdb::Activity {
        network: "local".into(),
        key: "native:0xabc:in".into(),
        direction: "in".into(),
        asset: "QUAI".into(),
        amount: "12500000000000000000".into(),
        address: "0x00F41a2B3c4D5e6F7a8B9c0D1e2F3a4B5c6D804B".into(),
        tx_hash: Some("0xabc".into()),
        block: Some(10),
        detail: serde_json::json!({}),
        observed: wallet_core::registry::now(),
    });
    app.observe_changes(&next);
    let said = app.toasts.last().map(|t| t.text.clone()).unwrap_or_default();
    assert!(said.starts_with("Received 12.5 QUAI") && said.contains("0x00F4"), "{said}");
    assert!(app.arrivals_unseen, "the rail marks Activity");
    assert!(matches!(app.hairline, Some((Signal::Arrival, _))), "runs in along the header");
    assert!(app.gutter_flash.is_some() && app.row_flash.contains_key("native:0xabc:in"));
    assert!(app.first_payment.is_some(), "the first payment this wallet ever received");
    let before = app.toasts.len();
    app.on_event(Ev::Notify { title: "Incoming payment".into(), body: "QUAI received".into() }, (120, 40));
    assert_eq!(app.toasts.len(), before, "said once");
    app.switch(Screen::Activity);
    assert!(!app.arrivals_unseen && app.first_payment.is_none(), "seen");
    // Dust from a stranger is never an arrival.
    app.dash = next.clone();
    let mut dust = next.clone();
    dust.activity.push(wallet_core::appdb::Activity {
        key: "native:0xdef:in".into(),
        amount: "1".into(),
        tx_hash: Some("0xdef".into()),
        ..next.activity[0].clone()
    });
    app.observe_changes(&dust);
    assert!(!app.arrivals_unseen);
}

/// The bell rings for news from outside only when nobody is watching, and not twice in ten
/// seconds; never for the user's own confirmations.
#[test]
fn the_bell_waits_for_an_empty_room() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.sound = true;
    app.focused = true;
    app.last_input = std::time::Instant::now();
    app.ring();
    assert!(!app.bell, "someone is looking");
    app.focused = false;
    app.ring();
    assert!(std::mem::take(&mut app.bell), "the window is in the background");
    app.ring();
    assert!(!app.bell, "not twice in ten seconds");
}

/// Screens changing fast skip the border draw-in; one on its own plays it.
#[test]
fn fast_hands_skip_the_draw_in() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.motion = wallet_core::config::Motion::Full;
    app.last_switch = None;
    app.start_transition();
    assert!(app.edge_intro.is_some(), "a switch on its own draws in");
    app.start_transition();
    assert!(app.edge_intro.is_none(), "a second one straight after just shows the screen");
}

/// Quitting leaves one plain line in the shell: the wallet's state, never a balance.
#[test]
fn quitting_leaves_a_plain_receipt() {
    let (_dir, mut app) = test_app(WalletKind::Watch);
    let line = app.quit_receipt().unwrap();
    assert!(line.starts_with("Quai Terminal closed") && !line.contains("keys"), "watch-only holds no keys: {line}");
    app.dash.ops.push(op("o1", "send_quai", wallet_core::appdb::OpStatus::Submitted));
    let line = app.quit_receipt().unwrap();
    assert!(line.ends_with("1 transaction still confirming (it doesn't need the wallet open)"), "{line}");
    assert!(!line.contains('$') && !line.contains("QUAI "), "{line}");
    app.meta = None;
    assert!(app.quit_receipt().is_none(), "nothing to say before a wallet exists");
}

/// A time lock opening is news: said in Home's attention panel until Accounts or Qi is opened.
#[test]
fn a_lock_opening_is_said_until_seen() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let lock = |unlocked: bool| wallet_core::track::LockItem {
        source: "QUAI→Qi conversion".into(),
        asset: "QI".into(),
        amount: "2.4".into(),
        unlock_height: Some(100),
        blocks_remaining: None,
        eta_secs: None,
        unlocked,
    };
    let mut dash = app.dash.clone();
    dash.network_id = "local".into();
    dash.refreshed_at = 1;
    dash.locks = vec![lock(false)];
    app.dash = dash.clone();
    let mut next = dash.clone();
    next.locks = vec![lock(true)];
    app.observe_changes(&next);
    assert_eq!(app.unlocked_news, vec!["2.4 Qi unlocked · spendable now".to_string()]);
    app.switch(Screen::Qi);
    assert!(app.unlocked_news.is_empty(), "seen");
}

/// Hold to sign (opt-in): one press on an armed, read review only starts the bar; Enter held
/// until it fills signs; a pause starts it over; any other key lets go.
#[test]
fn hold_to_sign_needs_enter_held() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.config.hold_to_sign = true;
    let op = "op-1";
    assert!(!app.held_to_sign(op), "the first press starts the bar");
    // Repeats inside the gap keep it; it signs once a second has passed.
    app.hold = Some((op.into(), std::time::Instant::now() - std::time::Duration::from_millis(1100), std::time::Instant::now()));
    assert!(app.held_to_sign(op), "held long enough");
    // A pause longer than a repeat starts it over.
    app.hold = Some((
        op.into(),
        std::time::Instant::now() - std::time::Duration::from_secs(3),
        std::time::Instant::now() - std::time::Duration::from_secs(1),
    ));
    assert!(!app.held_to_sign(op), "let go, then pressed again: starts over");
    // Another review is its own hold.
    assert!(!app.held_to_sign("op-2"));
}

/// The Konami code, typed in Help, gives the session the Genesis theme and saves nothing;
/// keys that don't carry it on still close Help as before.
#[test]
fn the_konami_code_in_help_gives_a_session_theme() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let saved = app.config.theme.clone();
    app.modal = Modal::Help;
    for code in [
        KeyCode::Up,
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char('b'),
        KeyCode::Char('a'),
    ] {
        press(&mut app, code);
        assert!(matches!(app.modal, Modal::Help), "{code:?} kept Help open");
    }
    assert_eq!(app.theme.name, "Genesis");
    assert_eq!(app.config.theme, saved, "never saved");
    press(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.modal, Modal::None), "any other key still closes Help");
}

/// Round heights are marked; the session's smallest head hash is kept for the Network screen.
#[test]
fn milestones_and_the_entropy_minimum() {
    use super::super::ui::milestone;
    assert!(milestone(5_000_000) && milestone(5_555_555) && milestone(10_000_000));
    assert!(!milestone(5_555_556) && !milestone(999_999) && !milestone(1_234_567));
    let (_dir, mut app) = test_app(WalletKind::Hd);
    let health = |height: u64, hash: &str| wallet_core::network::NodeHealth {
        network: "local".into(),
        chain_id: "1".into(),
        genesis: "0x".into(),
        identity_ok: true,
        height,
        head_hash: hash.into(),
        head_age_secs: None,
        gas_price: "0".into(),
        client_version: None,
        latency_ms: 1,
        order: Some(2),
    };
    for (h, hash) in [(10, "0x00ff"), (11, "0x000a"), (12, "0x0fff")] {
        let mut next = app.dash.clone();
        next.health = Some(health(h, hash));
        app.observe_changes(&next);
        app.dash = next;
    }
    assert_eq!(app.lowest_hash, Some(("0x000a".into(), 11)));
}

/// A transaction that could not be prepared says where the money is: nothing was sent.
#[test]
fn a_failed_prepare_says_nothing_was_sent() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.on_event(Ev::PrepareError("insufficient funds for gas".into()), (120, 40));
    let said = app.toasts.last().map(|t| t.text.clone()).unwrap_or_default();
    assert!(said.starts_with("insufficient balance for this amount plus the fee") && said.ends_with("nothing was sent"), "{said}");
}

/// A swap that landed reads like a fill ticket, against its quote.
#[test]
fn a_landed_swap_reads_like_a_fill_ticket() {
    let mut s = op("s1", "swap", wallet_core::appdb::OpStatus::Confirmed);
    s.asset = "QUAI".into();
    s.amount = "120000000000000000000".into();
    let e18 = |whole: u64, _tenths: u64| format!("{whole}000000000000000000");
    s.detail = serde_json::json!({"decimals": 18, "to_decimals": 18, "to_symbol": "WQI",
        "expected_out": e18(4200, 0), "minimum_out": e18(4179, 0), "actual_out": e18(4205, 0)});
    let said = super::events::swap_receipt(&s);
    assert!(said.starts_with("Swap landed · 120 QUAI → 4,205 WQI") && said.ends_with("0.12% better than quoted"), "{said}");
    s.detail["actual_out"] = serde_json::json!(e18(4195, 0));
    let said = super::events::swap_receipt(&s);
    assert!(said.ends_with("0.12% under the quote, inside your 0.5%"), "{said}");
    s.detail["actual_out"] = serde_json::Value::Null;
    assert_eq!(super::events::swap_receipt(&s), "Swap landed · QUAI → WQI", "older operations lack the amounts");
}

/// The recovery phrase stays up until it is closed on purpose; a review the lock discarded is
/// said once the wallet opens again.
#[test]
fn the_phrase_closes_on_purpose_and_a_locked_away_review_is_said() {
    let (_dir, mut app) = test_app(WalletKind::Hd);
    app.modal = Modal::Secret { text: zeroize::Zeroizing::new("abandon ability".into()), title: "phrase".into() };
    press(&mut app, KeyCode::Char('x'));
    assert!(matches!(app.modal, Modal::Secret { .. }), "a stray key leaves it up");
    press(&mut app, KeyCode::Esc);
    assert!(matches!(app.modal, Modal::None));
    // A review open when the wallet locks is discarded with the keys, and said after.
    app.on_event(review("r1", "send_quai"), (120, 40));
    assert!(matches!(app.modal, Modal::Review(_)));
    app.enter_lock(None);
    app.show_unlocked();
    let said = app.toasts.last().map(|t| t.text.clone()).unwrap_or_default();
    assert!(said.contains("discarded") && said.contains("nothing was signed"), "{said}");
}
