//! Rendering. Widgets use semantic theme roles only; color never carries meaning alone.

use super::app::{self, ACTIONS, App, FieldKind, Modal, OnboardKind, Onboarding, Picker, SETTINGS, Screen};
use super::terminal::Tier;
use super::theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Padding, Paragraph, Row, Sparkline, Table, Wrap};
use wallet_core::amount;
use wallet_core::appdb::{Activity, OpStatus, Operation};
use wallet_core::config::Motion;
use wallet_core::sdk::U256;
use wallet_core::session::{short_address, short_code};
use wallet_core::track::{describe, human_duration};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner() -> &'static str {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    SPINNER[(ms / 80) as usize % SPINNER.len()]
}

pub(crate) fn q(v: U256) -> String {
    amount::group_thousands(&amount::format_amount_short(v, 18, 4))
}

pub(crate) fn qi(v: U256) -> String {
    amount::group_thousands(&amount::qi(v))
}

/// How long ago a unix time was: `now`, `12m`, `2h 30m`, `3d 4h`. A time that was never known
/// (zero) is `—`, never "56 years ago".
pub(crate) fn ago(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let age = wallet_core::registry::now().saturating_sub(secs);
    if age < 60 { "now".into() } else { human_duration(age) }
}

/// [`ago`] in at most four cells, for a narrow column: `now`, `45s`, `12m`, `10h`, `3d`, `5w`.
pub(crate) fn ago_short(secs: u64) -> String {
    if secs == 0 {
        return "—".into();
    }
    let age = wallet_core::registry::now().saturating_sub(secs);
    match age {
        0..60 => "now".into(),
        60..3_600 => format!("{}m", age / 60),
        3_600..86_400 => format!("{}h", age / 3_600),
        86_400..1_209_600 => format!("{}d", age / 86_400),
        _ => format!("{}w", age / 604_800),
    }
}

/// Plain-language outcome of an operation kind, shown in every review.
fn review_story(kind: &str) -> Vec<&'static str> {
    match kind {
        "convert_quai_to_qi" => vec![
            "signed and broadcast; included within a few blocks",
            "if the block's shared discount exceeds your slippage, it refunds (the fee is spent)",
            "otherwise Qi arrives time-locked; the locks screen counts down to spendable",
        ],
        "convert_qi_to_quai" => {
            vec!["signed and broadcast; included within a few blocks", "QUAI arrives time-locked in the account (locks screen)"]
        }
        "send_qi" => vec![
            "each output lands on a fresh one-time address",
            "the recipient finds it with their payment code (mailbox or channel scan)",
        ],
        "wrap_qi" => vec!["Qi moves into the wrapper; once settled, claim WQI (wrap screen, m)"],
        "nft_list" | "nft_reprice" => vec![
            "a Zora ask goes live on-chain; Bazarr shows it within a minute",
            "the item stays in your wallet until someone buys it at this price",
            "when it sells, the proceeds arrive and the wallet notifies you (NFT sold)",
        ],
        "nft_unlist" => vec!["the ask is removed on-chain; nobody can buy the item at the old price"],
        "unwrap_wqi" => vec!["WQI is burned; Qi returns after the protocol lock"],
        "fill_gap" => vec!["uses the unused nonce; transactions queued behind it can then be mined"],
        "aggregate_qi" | "sweep_qi" => vec!["coins merge into fewer outputs you own; aggregation must be first in a block, so it may wait"],
        _ => vec!["signed and broadcast; included within a few blocks and tracked on activity (3)"],
    }
}

pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

/// Hairline panel with one cell of horizontal padding; only the focused panel gets the accent.
pub(crate) fn panel<'a>(t: &Theme, title: &str, focused: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(t.border(focused))
        .padding(Padding::horizontal(1))
        .title(if focused {
            Span::styled(format!(" ▸ {title} "), t.strong_style().fg(t.focus))
        } else {
            Span::styled(format!(" {title} "), t.dim_style())
        })
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2)).max(1);
    let h = height.min(area.height.saturating_sub(2)).max(1);
    Rect::new(area.x + (area.width.saturating_sub(w)) / 2, area.y + (area.height.saturating_sub(h)) / 2, w, h)
}

/// Break prose into lines of at most `width` columns, on spaces.
///
/// `Paragraph`'s own `Wrap` already does this when rendering; this exists so a modal can size
/// itself to prose before it renders, and so a wrapped block can be styled line by line.
pub(crate) fn textwrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let needed = if line.is_empty() { word.chars().count() } else { line.chars().count() + 1 + word.chars().count() };
        if needed > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    if lines.is_empty() { vec![String::new()] } else { lines }
}

pub(crate) fn darken(c: Color, amount: f32) -> Color {
    match c {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| (f32::from(v) * (1.0 - amount)) as u8;
            Color::Rgb(f(r), f(g), f(b))
        }
        other => other,
    }
}

/// Raised modal surface with a soft one-cell shadow. Returns the inner area.
pub(crate) fn modal_frame(f: &mut Frame, rect: Rect, t: &Theme, title: &str) -> Rect {
    let area = f.area();
    // A soft shadow: heavy on dark themes, a light tint on light ones.
    let shadow = darken(t.surface, if t.light { 0.08 } else { 0.45 });
    let buf = f.buffer_mut();
    for y in rect.y + 1..=(rect.y + rect.height).min(area.bottom().saturating_sub(1)) {
        let x = rect.x + rect.width;
        if x < area.right()
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_bg(shadow);
        }
    }
    for x in rect.x + 1..=(rect.x + rect.width).min(area.right().saturating_sub(1)) {
        let y = rect.y + rect.height;
        if y < area.bottom()
            && let Some(cell) = buf.cell_mut((x, y))
        {
            cell.set_bg(shadow);
        }
    }
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(t.focus).bg(t.raised))
        .style(Style::default().bg(t.raised).fg(t.text))
        .padding(Padding::new(2, 2, 1, 0))
        .title(Span::styled(format!(" {title} "), t.strong_style().fg(t.focus).bg(t.raised)));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

pub fn status_glyph(s: OpStatus) -> &'static str {
    match s {
        OpStatus::Confirmed | OpStatus::Settled => "✓",
        OpStatus::Failed => "×",
        OpStatus::Refunded => "↩",
        OpStatus::Unknown => "?",
        OpStatus::Replaced => "»",
        OpStatus::Cancelled => "–",
        OpStatus::Locked => "◕",
        _ => "○",
    }
}

pub(crate) fn status_style(t: &Theme, s: OpStatus) -> Style {
    let c = match s {
        OpStatus::Confirmed | OpStatus::Settled => t.ok,
        OpStatus::Failed | OpStatus::Refunded => t.danger,
        OpStatus::Unknown => t.attention,
        OpStatus::Cancelled | OpStatus::Replaced => t.dim,
        _ => t.pending,
    };
    Style::default().fg(c)
}

pub(crate) fn kind_icon(kind: &str) -> &'static str {
    match kind {
        k if k.starts_with("convert") => "↔",
        k if k.contains("unwrap") => "−",
        k if k.contains("wrap") || k.contains("claim") => "+",
        k if k.contains("approve") || k.contains("revoke") => "±",
        k if k.contains("aggregate") || k.contains("sweep") => "≋",
        k if k.contains("notify") => "@",
        _ => "↗",
    }
}

/// Centered empty state: glyph, one line, key hints.
pub(crate) fn empty_state(f: &mut Frame, area: Rect, t: &Theme, glyph: &str, text: &str, hints: &[(&str, &str)]) {
    let mut lines = vec![Line::from(Span::styled(format!("{glyph}  {text}"), t.dim_style()))];
    if !hints.is_empty() {
        let mut spans = Vec::new();
        for (i, (k, v)) in hints.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled("  ·  ", t.dim_style()));
            }
            spans.push(Span::styled(*k, t.strong_style().fg(t.focus)));
            spans.push(Span::styled(format!(" {v}"), t.dim_style()));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(spans));
    }
    let w = lines.iter().map(|line| line.width()).max().unwrap_or(1).min(area.width as usize).max(1) as u16;
    let h = lines.iter().map(|line| line.width().max(1).div_ceil(w as usize)).sum::<usize>().min(area.height as usize) as u16;
    let rect = Rect::new(area.x + area.width.saturating_sub(w) / 2, area.y + area.height.saturating_sub(h) / 2, w.min(area.width), h);
    f.render_widget(Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false }), rect);
}

// ---------------------------------------------------------------- big digits

const DIGITS: [[&str; 3]; 10] = [
    ["█▀█", "█ █", "▀▀▀"],
    ["▀█ ", " █ ", "▀▀▀"],
    ["▀▀█", "█▀▀", "▀▀▀"],
    ["▀▀█", " ▀█", "▀▀▀"],
    ["█ █", "▀▀█", "  ▀"],
    ["█▀▀", "▀▀█", "▀▀▀"],
    ["█▀▀", "█▀█", "▀▀▀"],
    ["▀▀█", "  █", "  ▀"],
    ["█▀█", "█▀█", "▀▀▀"],
    ["█▀█", "▀▀█", "▀▀▀"],
];

/// Three-row block digits for the whole part of a grouped number (e.g. "179,071").
pub fn big_digits(text: &str) -> [String; 3] {
    let mut rows = [String::new(), String::new(), String::new()];
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            for r in &mut rows {
                r.push(' ');
            }
        }
        match c {
            d @ '0'..='9' => {
                let g = DIGITS[d as usize - '0' as usize];
                for (r, part) in rows.iter_mut().zip(g) {
                    r.push_str(part);
                }
            }
            ',' => {
                // Thousands as a thin gap; a baseline block would read as a decimal point.
                for r in &mut rows {
                    r.push(' ');
                }
            }
            _ => {
                for r in &mut rows {
                    r.push(' ');
                }
            }
        }
    }
    rows
}

/// A balance "hero": big whole part, dim fraction and unit, ledger stripe.
/// Whether both balances fit as block digits (they share one size).
pub(crate) fn hero_fits(width: u16, height: u16, values: &[&str]) -> bool {
    height >= 5 && values.iter().all(|v| big_digits(v.split('.').next().unwrap_or(v))[0].chars().count() as u16 + 12 <= width)
}

// ---------------------------------------------------------------- frame

/// Draw one frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    app.eco.inline_icons.borrow_mut().clear();
    draw_frame(f, app);
    let t = app.theme.clone();
    legible_selection(f.buffer_mut(), &t);
    super::edge::paint(app, f.buffer_mut(), &t);
    super::images::place_inline_icons(app, f.buffer_mut(), &t);
}

/// Colored text on the selection highlight (amounts, addresses, statuses) keeps its theme color
/// only while it stays readable; otherwise it switches to the strong text color.
fn legible_selection(buf: &mut Buffer, t: &Theme) {
    let (Color::Rgb(sr, sg, sb), Color::Rgb(tr, tg, tb)) = (t.selection, t.strong) else { return };
    let area = buf.area;
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buf.cell_mut((x, y))
                && cell.bg == t.selection
                && let Color::Rgb(r, g, b) = cell.fg
                && super::theme::contrast((r, g, b), (sr, sg, sb)) < 4.5
            {
                cell.fg = Color::Rgb(tr, tg, tb);
            }
        }
    }
}

fn draw_frame(f: &mut Frame, app: &mut App) {
    let t = app.theme.clone();
    let area = f.area();
    f.render_widget(Block::default().style(t.base()), area);
    app.qr_rect = None;

    if area.width < 60 || area.height < 18 {
        let p = Paragraph::new(vec![
            Line::from(Span::styled("◆ quai-terminal", t.strong_style().fg(t.quai))),
            Line::from("Make the terminal at least 60×18."),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(p, centered(area, area.width, 3));
        return;
    }
    if app.onboarding.is_some() {
        draw_onboarding(f, app, &t, area);
        draw_toasts(f, app, &t, area);
        return;
    }
    if app.locked {
        draw_lock(f, app, &t, area);
        draw_modal(f, app, &t, area);
        draw_toasts(f, app, &t, area);
        return;
    }

    let [header, _rule, body, footer] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(0), Constraint::Min(5), Constraint::Length(1)]).areas(area);
    // Trader: Markets and the swap card side by side, on a terminal wide enough to hold both.
    app.trader = match app.config.layout.as_str() {
        "trader" => body.width >= 140,
        "auto" => body.width >= 200,
        _ => false,
    };
    let (nav, content) = if body.width >= 100 && app.config.layout != "focus" {
        let [n, m] = Layout::horizontal([Constraint::Length(20), Constraint::Min(40)]).areas(body);
        (Some(n), m)
    } else {
        (None, body)
    };
    draw_header(f, app, &t, header, nav.is_none());
    if let Some(n) = nav {
        draw_nav(f, app, &t, n);
    }
    let section = app.screen.section();
    let main = if section.tab_labels(&app.config.features).len() > 1 && content.height > 8 {
        let [tabs, m] = Layout::vertical([Constraint::Length(1), Constraint::Min(4)]).areas(content);
        super::views::draw_tabs(f, app, &t, tabs);
        m
    } else {
        content
    };
    // The pinned chat docks beside every screen but the Board (which shows it already): a column
    // on the right when there is width to spare, a strip along the bottom when there is height.
    app.dock_shown = false;
    let main = match app.eco.board.pin.clone() {
        Some(pin) if app.screen != Screen::Board && app.config.features.messaging => {
            if main.width >= 150 {
                let [m, dock] = Layout::horizontal([Constraint::Min(80), Constraint::Length(46)]).areas(main);
                super::views::draw_chat_dock(f, app, &t, dock, &pin);
                app.dock_shown = true;
                m
            } else if main.height >= 30 {
                let [m, dock] = Layout::vertical([Constraint::Min(20), Constraint::Length(9)]).areas(main);
                super::views::draw_chat_dock(f, app, &t, dock, &pin);
                app.dock_shown = true;
                m
            } else {
                main
            }
        }
        _ => main,
    };
    // While the chat has the keyboard, no pane of the screen is drawn as focused: one lit panel.
    let pane = app.pane;
    if app.dock_focus {
        app.pane = usize::MAX;
    }
    match app.detail.last() {
        Some(d) => super::views::draw_detail(f, app, &t, main, &d.clone()),
        None => match app.screen {
            Screen::Home => super::views::draw_home(f, app, &t, main),
            Screen::Markets | Screen::Swap if app.trader => {
                let [left, right] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(main);
                super::views::draw_markets(f, app, &t, left);
                super::views::draw_swap(f, app, &t, right);
            }
            Screen::Markets => super::views::draw_markets(f, app, &t, main),
            Screen::Accounts => draw_accounts(f, app, &t, main),
            Screen::Activity => draw_activity(f, app, &t, main),
            Screen::Qi => draw_qi(f, app, &t, main),
            Screen::Contacts => draw_payments(f, app, &t, main, false),
            Screen::Channels => draw_payments(f, app, &t, main, true),
            Screen::Board => super::views::draw_board(f, app, &t, main),
            Screen::Wallets => super::views::draw_wallets(f, app, &t, main),
            Screen::Swap => super::views::draw_swap(f, app, &t, main),
            Screen::Pools => super::views::draw_pools(f, app, &t, main),
            Screen::Convert => super::views::draw_convert_card(f, app, &t, main),
            Screen::Wrap => super::views::draw_wrap_card(f, app, &t, main),
            Screen::Locks => draw_locks(f, app, &t, main),
            Screen::Launches => super::views::draw_launches(f, app, &t, main),
            Screen::Pnl => super::views::draw_pnl(f, app, &t, main),
            Screen::Collected => super::views::draw_collected(f, app, &t, main),
            Screen::Explore => super::views::draw_explore(f, app, &t, main),
            Screen::Listings => super::views::draw_listings(f, app, &t, main),
            Screen::Network => draw_node(f, app, &t, main),
            Screen::Settings => draw_settings(f, app, &t, main),
            Screen::DataSources => super::views::draw_data_sources(f, app, &t, main),
        },
    }
    app.pane = pane;
    let elapsed = tachyonfx::Duration::from_millis(app.last_frame.elapsed().as_millis().min(50) as u32);
    if let Some(fx) = app.transition.as_mut() {
        fx.process(elapsed, f.buffer_mut(), main);
        if fx.done() {
            app.transition = None;
        }
    }
    let modal_open = !matches!(app.modal, Modal::None);
    // Decorative effects never draw over modals (reviews, secrets, forms).
    if modal_open {
        app.ceremony = None;
        app.celebration = None;
        app.ambient = None;
    }
    if let Some(c) = app.ceremony.as_mut() {
        let rect = centered(main, 72, 8);
        f.render_widget(Clear, rect);
        f.render_widget(Block::default().style(t.base()), rect);
        if c.step() {
            c.render(rect, f.buffer_mut(), t.base().fg(t.focus));
        } else {
            app.ceremony = None;
        }
    }
    if let Some(c) = app.ambient.as_mut() {
        f.render_widget(Clear, main);
        f.render_widget(Block::default().style(t.base()), main);
        if c.step() {
            c.render(main, f.buffer_mut(), t.base().fg(t.ok));
        } else {
            app.ambient = None;
            // `:poem`: the hash rain gives way to the haiku.
            if let Some(haiku) = app.poem_haiku.take() {
                let args = super::fx::theme_args("decrypt", &t);
                app.ambient =
                    super::fx::Ceremony::with_args("decrypt", &args, &haiku, 100, 30, 300).map(|c| c.at_speed(super::fx::LOCK_SPEED));
            }
        }
    }
    if let Some(c) = app.celebration.as_mut() {
        // Top-right corner of the main area, clear of balances on the left.
        let (w, h) = (44.min(main.width), 11.min(main.height));
        let rect = Rect::new(main.right().saturating_sub(w + 1), main.y + 1, w, h);
        if c.step() {
            c.render(rect, f.buffer_mut(), t.base().fg(t.ok));
        } else {
            app.celebration = None;
        }
    }
    draw_footer(f, app, &t, footer);
    draw_modal(f, app, &t, area);
    draw_toasts(f, app, &t, area);
}

fn draw_header(f: &mut Frame, app: &App, t: &Theme, area: Rect, show_screen: bool) {
    let d = &app.dash;
    let sep = || Span::styled(" │ ", t.dim_style());
    let name = app.meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
    // The wallet's name is the one thing on this bar that says whose money is on screen, so it
    // stands in capitals and the accent colour rather than in the run of grey beside it.
    let shown_name = if name.chars().count() <= 22 { name.to_uppercase() } else { name };
    let mut spans = vec![
        Span::raw(" "),
        super::images::native_span(app, t, "quai"),
        Span::raw(" "),
        Span::styled(shown_name, t.strong_style().fg(t.focus).add_modifier(Modifier::BOLD)),
    ];
    // The balance rides beside the name, rounded: this is the glance figure, not the ledger.
    // `$` hides it, for a room with other people in it.
    // Only where the bar has room for it: the breadcrumb and the node's state come first.
    if app.config.balance_in_bar && area.width >= 120 && !app.locked && !d.accounts.is_empty() {
        let quai = d.accounts.iter().fold(wallet_core::sdk::U256::ZERO, |s, a| s.saturating_add(a.balance));
        let shown = wallet_core::amount::group_thousands(&wallet_core::amount::format_amount_short(quai, 18, 2));
        spans.push(Span::styled(format!("  {shown} QUAI"), t.strong_style()));
    }
    spans.push(sep());
    if show_screen {
        // Narrow terminals: the nav rail collapses into a section strip.
        for s in app.sections() {
            let active = s == app.screen.section();
            spans.push(Span::styled(
                format!("{}{} ", s.key(), if active { format!(" {}", s.title()) } else { String::new() }),
                if active { t.strong_style().fg(t.focus) } else { t.dim_style() },
            ));
        }
        spans.push(sep());
    }
    let crumbs = app.breadcrumb();
    let last = crumbs.len().saturating_sub(1);
    for (i, c) in crumbs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" › ", t.dim_style()));
        }
        spans.push(Span::styled(truncate(c, 24), if i == last { t.strong_style().fg(t.focus) } else { Style::default().fg(t.focus) }));
    }
    spans.push(sep());
    let beating = app.beat.is_some_and(|b| b.elapsed().as_millis() < 700);
    let stale = d.health.as_ref().and_then(|h| h.head_age_secs).is_some_and(|s| s > 90);
    let mismatch = d.health.as_ref().is_some_and(|h| !h.identity_ok);
    // Every node state has its own glyph and word, never color alone.
    let (dot, word, dot_color) = match (&d.node_error, &d.health) {
        (Some(_), _) => ("×", "offline ", t.danger),
        _ if mismatch => ("×", "WRONG CHAIN ", t.danger),
        (None, None) => ("○", "", t.dim),
        _ if stale => ("◕", "stale ", t.pending),
        _ if beating => ("◉", "", t.ok),
        _ => ("●", "", darken(t.ok, 0.25)),
    };
    spans.push(Span::styled(format!("{dot} {word}"), Style::default().fg(dot_color)));
    let net = if d.network_name.is_empty() { app.network_id.clone() } else { d.network_name.clone() };
    let mainnet = d.network_id == "mainnet" || (d.network_id.is_empty() && app.network_id == "mainnet");
    spans.push(Span::styled(net, if mainnet { Style::default().fg(t.link) } else { t.strong_style().fg(t.attention) }));
    if let Some(h) = &d.health {
        spans.push(Span::styled(format!("  #{}", amount::group_thousands(&h.height.to_string())), t.dim_style()));
    }
    if let Some(e) = &d.node_error {
        spans.push(Span::styled(format!("  {}", truncate(&app::friendly_error(e), 40)), Style::default().fg(t.danger)));
    }
    if let Some(m) = &app.meta {
        if m.kind == wallet_core::registry::WalletKind::Watch {
            spans.push(sep());
            spans.push(Span::styled("watch-only", Style::default().fg(t.attention)));
        } else if m.kind == wallet_core::registry::WalletKind::Hd && !m.backed_up {
            spans.push(sep());
            spans.push(Span::styled("! phrase not verified", Style::default().fg(t.attention)));
        }
    }
    let mut right = Vec::new();
    if let Some(b) = &app.busy {
        right.push(Span::styled(format!("{} {b} ", spinner()), Style::default().fg(t.pending)));
    }
    let unread = d.notifications.iter().filter(|n| !n.read).count();
    if unread > 0 {
        right.push(Span::styled(format!("● {unread} unread "), Style::default().fg(t.attention)));
        right.push(Span::styled("N  ", t.strong_style().fg(t.focus)));
    }
    if app.can_sign() {
        match app.autolock_remaining() {
            Some(s) if app.dash.unlocked => {
                let c = if s < 60 { t.attention } else { t.ok };
                right.push(Span::styled(
                    format!("● unlocked · {} ", if s < 60 { format!("{s}s") } else { format!("{}m", s.div_ceil(60)) }),
                    Style::default().fg(c),
                ));
            }
            _ if app.dash.unlocked => right.push(Span::styled("● unlocked ", Style::default().fg(t.ok))),
            _ => right.push(Span::styled("○ locked ", t.dim_style())),
        }
    }
    // The right side wins; the left side is cut short instead of overlapping it.
    let right_line = Line::from(right);
    let right_w = (right_line.width() as u16).min(area.width);
    let left_area = Rect { width: area.width.saturating_sub(right_w + 1), ..area };
    f.render_widget(Block::default().style(Style::default().bg(t.raised)), area);
    f.render_widget(Paragraph::new(Line::from(spans)), left_area);
    f.render_widget(Paragraph::new(right_line).alignment(Alignment::Right), area);
}

fn draw_nav(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mut lines = Vec::new();
    let current = app.screen.section();
    for (i, s) in app.sections().iter().enumerate() {
        if i > 0 && *s == app::Section::System {
            lines.push(Line::from(""));
        }
        let active = *s == current;
        let style = if active { t.selected() } else { t.text_style() };
        let dot = match s {
            app::Section::Home => Span::styled("•", Style::default().fg(t.quai)),
            app::Section::Nfts => Span::styled("◧", Style::default().fg(t.qi)),
            _ => Span::raw(" "),
        };
        lines.push(Line::from(vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!("{} ", s.key()), if active { style } else { t.dim_style() }),
            Span::styled(format!("{:<13}", s.title()), style),
            dot,
        ]));
        if active && s.tab_labels(&app.config.features).len() > 1 {
            let active_tab = if *s == app::Section::Activity {
                app::ActivityFilter::ALL.iter().position(|f| *f == app.activity_filter).unwrap_or(0)
            } else {
                s.screens(&app.config.features).iter().position(|v| *v == app.screen).unwrap_or(0)
            };
            for (ti, label) in s.tab_labels(&app.config.features).iter().enumerate() {
                let on = ti == active_tab;
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(if on { "› " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(label.to_string(), if on { t.strong_style() } else { t.dim_style() }),
                ]));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  [ ] tabs", t.dim_style().add_modifier(Modifier::ITALIC))));
    let block = Block::default().borders(Borders::RIGHT).border_type(BorderType::Plain).border_style(t.border(false));
    f.render_widget(Paragraph::new(lines).block(block), Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area });
}

fn draw_footer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mut hints: Vec<(&str, &str)> = match app.detail.last() {
        Some(d) => {
            let mut h = super::views::detail_hints(app, d);
            h.truncate(app::FOOTER_HINTS - 1);
            h.push(("esc", "back"));
            h
        }
        None => app::context_hints(app),
    };
    if app.dash.notifications.iter().any(|n| !n.read) {
        hints.insert(0, ("N", "notifications"));
    }
    hints.push((":", "palette"));
    // Lock, quit and the rest of this screen's keys live under ?.
    let help = [("?", "more")];
    let width = |h: &[(&str, &str)]| h.iter().map(|(k, v)| k.chars().count() + v.chars().count() + 3).sum::<usize>();
    // Narrow: screen hints give way first; the palette, the way to everything else, stays.
    while hints.len() > 1 && width(&hints) + width(&help) > area.width as usize {
        hints.remove(hints.len() - 2);
    }
    let mut spans = Vec::new();
    for (k, v) in hints.iter().chain(help.iter()) {
        spans.push(Span::styled(format!(" {k}"), t.strong_style().fg(t.focus)));
        spans.push(Span::styled(format!(" {v} "), t.dim_style()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(Style::default().bg(t.raised)), area);
}

/// One line in the bottom-right corner while a transaction is sent and not yet mined: a turning
/// spinner, what it is, and how long it has been waiting. It never takes a second row, and the
/// toasts stack above it.
fn draw_pending(f: &mut Frame, app: &App, t: &Theme, area: Rect) -> u16 {
    let waiting = app.confirming_ops();
    let Some(oldest) = waiting.first() else { return 0 };
    let more = waiting.len().saturating_sub(1);
    let summary = match more {
        0 => wallet_core::track::describe(oldest),
        n => format!("{} +{n}", wallet_core::track::describe(oldest)),
    };
    let max = (area.width as usize).saturating_sub(18).min(52);
    let text = format!("{} · {}", truncate(&summary, max), super::views::flow_age(oldest.updated));
    // A still glyph where motion is off: the age keeps counting either way.
    let glyph = if app.motion().effects() { spinner() } else { "◌" };
    let w = (text.chars().count() as u16 + 6).min(area.width);
    let rect = Rect::new(area.right().saturating_sub(w + 1), area.bottom().saturating_sub(1), w, 1);
    f.render_widget(Clear, rect);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("▌", Style::default().fg(t.pending)),
            Span::styled(format!("{glyph} "), Style::default().fg(t.pending)),
            Span::styled(text, t.text_style()),
        ]))
        .style(Style::default().bg(t.raised)),
        rect,
    );
    1
}

fn draw_toasts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // Newest on the footer row (right side), older ones stacked above it — above the pending
    // pill when one is showing.
    let mut y = area.bottom().saturating_sub(1 + draw_pending(f, app, t, area));
    for toast in app.toasts.iter().rev() {
        let max = (area.width as usize).saturating_sub(10).min(90);
        let text = truncate(&toast.text, max);
        let w = (text.chars().count() as u16 + 6).min(area.width);
        let rect = Rect::new(area.right().saturating_sub(w + 1), y, w, 1);
        let (glyph, color) = if toast.error { ("×", t.danger) } else { ("✓", t.ok) };
        f.render_widget(Clear, rect);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▌", Style::default().fg(color)),
                Span::styled(format!("{glyph} "), Style::default().fg(color)),
                Span::styled(format!("{text} "), Style::default().fg(t.strong)),
            ]))
            .style(Style::default().bg(t.raised)),
            rect,
        );
        if y <= area.y + 2 {
            break;
        }
        y -= 1;
    }
}

// ---------------------------------------------------------------- screens

pub(crate) fn incoming_text(a: &Activity) -> String {
    let v: U256 = a.amount.parse().unwrap_or_default();
    match a.asset.as_str() {
        "QI" => format!("{} Qi", qi(v)),
        "QUAI" => format!("{} QUAI", q(v)),
        other => {
            let dec = a.detail.get("decimals").and_then(|d| d.as_u64()).unwrap_or(18) as u8;
            format!("{} {other}", amount::format_amount_short(v, dec, 4))
        }
    }
}

/// Contact whose payment code or address is `key` (addresses compare case-insensitively).
pub(crate) fn contact_matching<'a>(app: &'a App, key: &str) -> Option<&'a str> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    app.dash
        .contacts
        .iter()
        .find(|c| c.payment_code.as_deref() == Some(key) || c.address.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(key)))
        .map(|c| c.name.as_str())
}

/// The contact an operation was with: its payment-code peer, else its counterparty.
pub(crate) fn op_contact<'a>(app: &'a App, op: &Operation) -> Option<&'a str> {
    op.detail["peer"].as_str().and_then(|p| contact_matching(app, p)).or_else(|| contact_matching(app, &op.counterparty))
}

/// The contact who sent an incoming payment, when the channel is known.
pub(crate) fn activity_contact<'a>(app: &'a App, a: &Activity) -> Option<&'a str> {
    if let Some(peer) = a.detail["peer"].as_str() {
        return contact_matching(app, peer);
    }
    // Rows recorded before the full code was kept only carry `payment from <short code>`.
    let short = a.detail["origin"].as_str()?.strip_prefix("payment from ")?;
    app.dash.contacts.iter().find(|c| c.payment_code.as_deref().is_some_and(|code| short_code(code) == short)).map(|c| c.name.as_str())
}

/// Icon for the token, NFT collection or native coin an activity row is about.
fn row_badge(app: &App, t: &Theme, symbol: &str, detail: &serde_json::Value) -> Option<Span<'static>> {
    let Some(contract) =
        ["contract", "token", "to_token"].iter().find_map(|k| detail[*k].as_str().filter(|c| c.starts_with("0x"))).map(str::to_lowercase)
    else {
        return match symbol.to_ascii_lowercase().as_str() {
            native @ ("quai" | "qi") => Some(super::images::native_span(app, t, native)),
            _ => None,
        };
    };
    // NFTs are badged by collection name; fungible tokens by symbol (their name may differ).
    let nft = detail["token_id"].as_str().is_some_and(|id| !id.is_empty());
    let name = detail["name"].as_str().filter(|n| nft && !n.is_empty()).unwrap_or(symbol);
    let icon = app.asset_icon_url(&contract);
    Some(super::images::badge_span(app, t, icon.as_deref(), name, &contract))
}

fn with_badge(badge: Option<Span<'static>>, text: String) -> Line<'static> {
    match badge {
        Some(b) => Line::from(vec![b, Span::raw(" "), Span::raw(text)]),
        None => Line::from(text),
    }
}

/// Confirmations shown as a tally until a mined operation reaches this depth.
pub(crate) const CONFIRM_TARGET: u64 = 5;

/// (confirmations, target) for a mined operation still short of the target.
pub(crate) fn confirmations(op: &Operation, head: u64) -> Option<(u64, u64)> {
    if !matches!(op.status, OpStatus::Confirmed | OpStatus::Settled | OpStatus::Settling | OpStatus::Locked) {
        return None;
    }
    let included = op.detail["included_block"].as_u64()?;
    let n = head.checked_sub(included)? + 1;
    (n < CONFIRM_TARGET + 1).then_some((n.min(CONFIRM_TARGET), CONFIRM_TARGET))
}

/// `━━━╍╍ 3/5`: filled segments per confirmation.
pub(crate) fn tally(n: u64, target: u64) -> String {
    format!("{}{} {n}/{target}", "━".repeat(n as usize), "╍".repeat(target.saturating_sub(n) as usize))
}

/// What is happening with an unfinished operation, and whether it waits on the user.
pub(crate) fn op_next_step(op: &Operation, head: u64) -> String {
    let unlock = op.detail["unlock_height"].as_u64().filter(|u| *u > head);
    match op.status {
        OpStatus::Prepared | OpStatus::Signed => "not sent yet · it is submitted when you approve the review".into(),
        // Both ledgers can be replaced now, so neither arm singles one out. A Qi replacement pays
        // its higher fee out of the transaction's own change rather than from the account.
        OpStatus::Submitted => "waiting to be mined · u speeds it up with a higher fee".into(),
        OpStatus::Unknown => "submission not confirmed yet · the wallet re-checks the chain on every refresh · u speeds it up".into(),
        OpStatus::Settling => "mined · waiting for the destination to settle · nothing to do".into(),
        OpStatus::Locked => match unlock {
            Some(u) => format!(
                "mined · locked by the protocol until block {} (~{}) · becomes spendable automatically · nothing to do",
                amount::group_thousands(&u.to_string()),
                wallet_core::track::human_duration((u - head) * 5)
            ),
            None => "mined · locked by the protocol · becomes spendable automatically · nothing to do".into(),
        },
        _ => String::new(),
    }
}

pub(crate) fn op_row_parts(app: &App, t: &Theme, op: &Operation) -> (Span<'static>, String, Span<'static>, String) {
    let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
    let status = match confirmations(op, head).filter(|(n, target)| n < target) {
        Some((n, target)) => Span::styled(format!("{} {}", status_glyph(op.status), tally(n, target)), Style::default().fg(t.pending)),
        None => Span::styled(format!("{} {}", status_glyph(op.status), op.status.as_str()), status_style(t, op.status)),
    };
    let icon_color = if op.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
    let what = match op_contact(app, op) {
        Some(name) if op.kind != "notify" => format!("{} → {name}", describe(op)),
        Some(name) => format!("mailbox notify to {name}"),
        None => describe(op),
    };
    (Span::styled(kind_icon(&op.kind), Style::default().fg(icon_color)), what, status, op.tx_hash.clone().unwrap_or_default())
}

/// Value and gas for a row, as detail lines: what the transaction carried in its native coin
/// (QUAI, or Qi on the UTXO ledger) and what it cost to send. Labels are the second element's
/// keys, so both detail views say the same thing.
pub(crate) fn cost_lines(app: &App, t: &Theme, op: Option<&Operation>, seen: Option<&Activity>) -> Vec<(&'static str, Span<'static>)> {
    use wallet_core::track::{native_text, op_cost};
    let dim = t.dim_style();
    if let Some(op) = op {
        let cost = op_cost(op);
        let value = match cost.value {
            Some(v) => Span::styled(cost.text(v), t.text_style()),
            None => Span::styled("—", dim),
        };
        let fee = match (cost.fee, cost.fee_final) {
            (Some(f), true) => Span::styled(cost.text(f), t.text_style()),
            (Some(f), false) => Span::styled(format!("up to {} · not mined yet", cost.text(f)), Style::default().fg(t.pending)),
            (None, _) => Span::styled("—", dim),
        };
        return vec![("value", value), (if cost.qi { "fee" } else { "gas" }, fee)];
    }
    let Some(a) = seen else { return Vec::new() };
    let incoming = a.direction == "in";
    let qi = a.asset == "QI";
    let known = a.tx_hash.as_ref().and_then(|h| app.eco.tx_costs.get(h));
    let reading = || Span::styled(format!("{} reading…", spinner()), dim);
    // A native row states its own value; a token row's native value comes from the transaction.
    let value = match (a.asset.as_str(), known) {
        ("QUAI" | "QI", _) => Span::styled(native_text(a.amount.parse().unwrap_or_default(), qi), t.text_style()),
        (_, Some(Ok(c))) => c.value.map_or_else(|| Span::styled("—", dim), |v| Span::styled(c.text(v), t.text_style())),
        (_, Some(Err(_))) => Span::styled("—", dim),
        (_, None) if a.tx_hash.is_some() => reading(),
        _ => Span::styled("—", dim),
    };
    let sender = if incoming { " · paid by the sender" } else { "" };
    let fee = match known {
        _ if qi => Span::styled(if incoming { "paid by the sender" } else { "—" }, dim),
        Some(Ok(c)) => c.fee.map_or_else(|| Span::styled("—", dim), |f| Span::styled(format!("{}{sender}", c.text(f)), t.text_style())),
        Some(Err(e)) => Span::styled(truncate(&app::friendly_error(e), 40), dim),
        None if a.tx_hash.is_some() => reading(),
        None => Span::styled("—", dim),
    };
    vec![("value", value), (if qi { "fee" } else { "gas" }, fee)]
}

/// The fee for a table row, compact: what was paid, `≤` the most it can cost while unmined.
/// Incoming rows are blank — their gas was the sender's.
fn fee_cell(app: &App, t: &Theme, op: Option<&Operation>, seen: Option<&Activity>) -> Span<'static> {
    if let Some(op) = op {
        let cost = wallet_core::track::op_cost(op);
        return match (cost.fee, cost.fee_final) {
            (Some(f), true) => Span::styled(cost.text(f), t.dim_style()),
            (Some(f), false) => Span::styled(format!("≤{}", cost.text(f)), Style::default().fg(t.pending)),
            (None, _) => Span::raw(""),
        };
    }
    match seen {
        Some(a) if a.direction == "out" => match a.tx_hash.as_ref().and_then(|h| app.eco.tx_costs.get(h)) {
            Some(Ok(c)) => c.fee.map_or_else(|| Span::raw(""), |f| Span::styled(c.text(f), t.dim_style())),
            _ => Span::raw(""),
        },
        _ => Span::raw(""),
    }
}

/// Activity rows. `compact` is the width the description may take: a narrow panel (Home) gets a
/// four-cell age, the description cut with an ellipsis where it has to be, and the status as its
/// mark alone — the full words, gas and hash are on the Activity screen.
pub(crate) fn activity_table_rows<'a>(app: &App, t: &Theme, limit: usize, labels: bool, compact: Option<usize>) -> Vec<Row<'a>> {
    let rows = app.activity_rows();
    let offset = if labels { app.view_offset() } else { 0 };
    rows.iter()
        .enumerate()
        .skip(offset)
        .take(limit.max(1))
        .map(|(i, (time, is_op, idx))| {
            let badge = if *is_op {
                let op = &app.dash.ops[*idx];
                row_badge(app, t, &op.asset, &op.detail)
            } else {
                let a = &app.dash.activity[*idx];
                row_badge(app, t, &a.asset, &a.detail)
            };
            let (icon, what, status, hash) = if *is_op {
                op_row_parts(app, t, &app.dash.ops[*idx])
            } else {
                let a = &app.dash.activity[*idx];
                let color = if a.asset == "QI" { t.qi } else { t.quai };
                let planted = app.dust_from_stranger(a);
                let outgoing = a.direction == "out";
                let (joiner, verb) = if outgoing { ("to", "✓ sent") } else { ("from", "✓ received") };
                let what = match activity_contact(app, a) {
                    Some(name) => format!("{} {joiner} {name}", super::views::activity_text(a)),
                    None => {
                        format!("{} {} {}", super::views::activity_text(a), if outgoing { "from" } else { "→" }, short_address(&a.address))
                    }
                };
                // Dust from an address never sent to is how a lookalike gets into the history: say so
                // where it sits, not only when someone tries to send to it.
                let status = if planted {
                    Span::styled("⚠ dust · likely a lookalike", Style::default().fg(t.attention))
                } else {
                    Span::styled(verb, Style::default().fg(t.ok))
                };
                (
                    Span::styled(if outgoing { "↗" } else { "↘" }, Style::default().fg(color)),
                    what,
                    status,
                    a.tx_hash.clone().unwrap_or_default(),
                )
            };
            let mut cells = Vec::new();
            if labels {
                cells.push(Cell::from(Span::styled(app::jump_label(i - offset).to_string(), Style::default().fg(t.focus))));
            }
            if let Some(width) = compact {
                let mark = status.content.chars().next().map(String::from).unwrap_or_default();
                let cells = vec![
                    Cell::from(Line::from(Span::styled(ago_short(*time), t.dim_style())).alignment(Alignment::Right)),
                    Cell::from(icon),
                    Cell::from(with_badge(badge, truncate(&what, width.saturating_sub(3)))),
                    Cell::from(Span::styled(mark, status.style)),
                ];
                return Row::new(cells);
            }
            cells.push(Cell::from(Span::styled(ago(*time), t.dim_style())));
            cells.push(Cell::from(icon));
            cells.push(Cell::from(with_badge(badge, what)));
            cells.push(Cell::from(status));
            let fee =
                if *is_op { fee_cell(app, t, app.dash.ops.get(*idx), None) } else { fee_cell(app, t, None, app.dash.activity.get(*idx)) };
            cells.push(Cell::from(Line::from(fee).alignment(Alignment::Right)));
            cells.push(Cell::from(Span::styled(if hash.is_empty() { String::new() } else { short_address(&hash) }, t.dim_style())));
            let row = Row::new(cells);
            // A row that just reached its confirmation target lights once, fading out (background
            // only; the text keeps its colors).
            let flash = is_op.then(|| app.row_flash.get(&app.dash.ops[*idx].id)).flatten().map(|s| s.elapsed().as_millis());
            if labels && i == app.selected {
                row.style(t.selected())
            } else if let Some(bg) = flash.and_then(|ms| super::edge::flash_bg(t, t.ok, ms)) {
                row.style(Style::default().bg(bg))
            } else {
                row
            }
        })
        .collect()
}

pub(crate) fn draw_accounts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let n_accounts = app.dash.accounts.len();
    let acct_area = area;
    let narrow = acct_area.width < 110;
    let block = panel(t, "quai accounts", true);
    let inner = block.inner(acct_area);
    f.render_widget(block, acct_area);
    if n_accounts == 0 {
        empty_state(f, inner, t, "○", "No accounts loaded yet.", &[("a", "add account")]);
    } else {
        let rows: Vec<Row> = app
            .dash
            .accounts
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i).to_string(), Style::default().fg(t.focus))),
                    Cell::from(Span::styled("▌", Style::default().fg(t.quai))),
                    Cell::from(a.label.clone()),
                    Cell::from(Span::styled(
                        if narrow { short_address(&a.address) } else { a.address.clone() },
                        Style::default().fg(t.link),
                    )),
                    Cell::from(Line::from(Span::styled(q(a.balance), t.strong_style().fg(t.quai))).alignment(Alignment::Right)),
                    Cell::from(if a.locked.is_zero() { String::new() } else { format!("◕ {}", q(a.locked)) }),
                    Cell::from(Span::styled(a.nonce.to_string(), t.dim_style())),
                ]);
                if i == app.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        let addr_w = if narrow { 13 } else { 44 };
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(14),
                Constraint::Length(addr_w),
                Constraint::Length(18),
                Constraint::Length(16),
                Constraint::Length(6),
            ],
        )
        .column_spacing(2)
        .header(
            Row::new(vec![
                Cell::from(""),
                Cell::from(""),
                Cell::from("label"),
                Cell::from("address"),
                Cell::from(Line::from(vec![super::images::native_span(app, t, "quai"), Span::raw(" QUAI")]).alignment(Alignment::Right)),
                Cell::from("locked"),
                Cell::from("nonce"),
            ])
            .style(t.dim_style()),
        );
        f.render_widget(table, inner);
    }
}

pub(crate) fn draw_activity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let (list, detail) = if area.width >= 120 {
        let [l, d] = Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
        (l, d)
    } else {
        let [l, d] = Layout::vertical([Constraint::Min(6), Constraint::Length(11)]).areas(area);
        (l, d)
    };
    let title = if app.jump_pending.is_some() {
        "activity · press a label".to_string()
    } else {
        format!("activity · {}", app.activity_filter.title().to_lowercase())
    };
    let block = panel(t, &title, true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    let rows = activity_table_rows(app, t, inner.height.saturating_sub(1) as usize, true, None);
    if app.activity_rows().is_empty() {
        empty_state(
            f,
            inner,
            t,
            "○",
            "Nothing here yet — sends, receipts and conversions will appear as they happen.",
            &[("r", "receive")],
        );
    } else {
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(8),
                Constraint::Length(1),
                Constraint::Min(24),
                Constraint::Length(12),
                Constraint::Length(15),
                Constraint::Length(13),
            ],
        )
        .header(
            Row::new(vec![
                Cell::from(""),
                Cell::from("when"),
                Cell::from(""),
                Cell::from("what"),
                Cell::from("status"),
                Cell::from(Line::from("gas").alignment(Alignment::Right)),
                Cell::from("tx"),
            ])
            .style(t.dim_style()),
        );
        f.render_widget(table, inner);
    }
    let rows = app.activity_rows();
    let mut lines: Vec<Line> = Vec::new();
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<12}"), t.dim_style()), Span::raw(v)]);
    match rows.get(app.selected) {
        Some((_, true, i)) => {
            let op = &app.dash.ops[*i];
            let contact = op_contact(app, op);
            lines.push(Line::from(vec![
                Span::styled(format!("{} ", kind_icon(&op.kind)), Style::default().fg(t.focus)),
                Span::styled(op_row_parts(app, t, op).1, t.strong_style()),
            ]));
            lines.push(Line::from(Span::styled(format!("{} {}", status_glyph(op.status), op.status.as_str()), status_style(t, op.status))));
            lines.push(kv("operation", op.id.clone()));
            lines.push(kv("from", op.account.clone()));
            if let Some(name) = contact {
                lines.push(kv("contact", name.to_string()));
            }
            lines.push(kv("to", op.counterparty.clone()));
            for (label, value) in cost_lines(app, t, Some(op), None) {
                lines.push(Line::from(vec![Span::styled(format!("{label:<12}"), t.dim_style()), value]));
            }
            if let Some(h) = &op.tx_hash {
                lines.push(kv("tx", h.clone()));
                if let Some(e) = &app.dash.explorer {
                    lines.push(kv("explorer", format!("{}/tx/{h}", e.trim_end_matches('/'))));
                }
            }
            if let Some(obj) = op.detail.as_object() {
                // `native_value` is stated above as the value, in QUAI rather than wei.
                for (k, v) in obj.iter().filter(|(k, _)| k.as_str() != "native_value").take(10) {
                    lines.push(kv(k, v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())));
                }
            }
            if !op.status.is_terminal() {
                let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("next  ", t.strong_style().fg(t.focus)),
                    Span::styled(op_next_step(op, head), t.text_style()),
                ]));
            }
        }
        Some((_, false, i)) => {
            let a = &app.dash.activity[*i];
            lines.push(Line::from(vec![
                Span::styled("↘ ", Style::default().fg(t.ok)),
                Span::styled(format!("{} {}", wallet_core::track::incoming_verb(a), incoming_text(a)), t.strong_style()),
            ]));
            if let Some(name) = activity_contact(app, a) {
                lines.push(kv("from", name.to_string()));
            }
            lines.push(kv("to", a.address.clone()));
            for (label, value) in cost_lines(app, t, None, Some(a)) {
                lines.push(Line::from(vec![Span::styled(format!("{label:<12}"), t.dim_style()), value]));
            }
            if let Some(h) = &a.tx_hash {
                lines.push(kv("tx", h.clone()));
            }
            if let Some(b) = a.block {
                lines.push(kv("block", amount::group_thousands(&b.to_string())));
            }
        }
        None => lines.push(Line::from(Span::styled("select a row to see details", t.dim_style()))),
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(panel(t, "detail", false)), detail);
}

pub(crate) fn draw_qi(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let Some(s) = &app.dash.qi else {
        let block = panel(t, "qi coins", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        empty_state(f, inner, t, spinner(), "Your coin purse hasn't been counted yet.", &[("S", "scan now")]);
        return;
    };
    let [summary, rest] = Layout::vertical([Constraint::Length(6), Constraint::Min(5)]).areas(area);
    // Stacked balance bar: spendable · locked · reserved.
    let block = panel(t, "qi balance", false);
    let inner = block.inner(summary);
    f.render_widget(block, summary);
    let total = u128::try_from(s.balance.total).unwrap_or(0).max(1) as f64;
    let part = |v: U256| (u128::try_from(v).unwrap_or(0) as f64 / total * f64::from(inner.width)).round() as usize;
    let (sp, lk, rs) = (part(s.balance.spendable), part(s.balance.locked), part(s.balance.reserved));
    let rest_w = (inner.width as usize).saturating_sub(sp + lk + rs);
    let legend = Line::from(vec![
        super::images::native_span(app, t, "qi"),
        Span::raw(" "),
        Span::styled("■ ", Style::default().fg(t.qi)),
        Span::styled(format!("spendable {}   ", qi(s.balance.spendable)), t.strong_style()),
        Span::styled("■ ", Style::default().fg(t.pending)),
        Span::styled(format!("locked {}   ", qi(s.balance.locked)), t.text_style()),
        Span::styled("■ ", Style::default().fg(t.attention)),
        Span::styled(format!("reserved {}", qi(s.balance.reserved)), t.text_style()),
        Span::styled(format!("   · checkpoint #{}", amount::group_thousands(&s.checkpoint_height.unwrap_or(0).to_string())), t.dim_style()),
    ]);
    let bar = Line::from(vec![
        Span::styled("█".repeat(sp), Style::default().fg(t.qi)),
        Span::styled("█".repeat(lk), Style::default().fg(t.pending)),
        Span::styled("█".repeat(rs), Style::default().fg(t.attention)),
        Span::styled("░".repeat(rest_w), t.dim_style()),
    ]);
    // Cash drawer: a slot per denomination held, stacked like notes in a till. The stack height
    // shows how many of that coin there are, so a wallet that needs consolidating *looks* like one:
    // tall stacks of small change on the left, a few big notes on the right.
    let values = wallet_core::sdk::consensus::Denomination::VALUES;
    let mut held = vec![0u64; values.len()];
    for c in &s.coins {
        if let Some(n) = held.get_mut(c.denomination as usize) {
            *n += 1;
        }
    }
    let stack = |n: u64| match n {
        0 => ' ',
        1 => '▁',
        2 => '▂',
        3..=4 => '▃',
        5..=8 => '▄',
        9..=16 => '▅',
        17..=32 => '▆',
        33..=64 => '▇',
        _ => '█',
    };
    let mut tray = vec![Span::styled("drawer ", t.dim_style())];
    let mut stacks = vec![Span::styled("       ", t.dim_style())];
    for (i, n) in held.iter().enumerate().filter(|(_, n)| **n > 0) {
        let lit = app.drawer_flash.get(&(i as u8)).and_then(|s| super::edge::flash_bg(t, t.qi, s.elapsed().as_millis()));
        let bg = lit.or_else(|| super::edge::tint(t, t.qi, 0.22)).unwrap_or(t.raised);
        let label = format!(" {} ×{n} ", amount::qi(U256::from(values[i])));
        // Small coins cost the most fee to spend, so they read in the attention color once a
        // stack is deep enough to be worth consolidating.
        let heavy = i <= 6 && *n >= 8;
        let fg = if heavy { t.attention } else { t.qi };
        stacks.push(Span::styled(format!("{:^width$}", stack(*n), width = label.chars().count()), Style::default().fg(fg).bg(bg)));
        stacks.push(Span::raw(" "));
        tray.push(Span::styled(label, t.strong_style().bg(bg)));
        tray.push(Span::raw(" "));
    }
    if held.iter().all(|n| *n == 0) {
        tray.push(Span::styled("empty · coins arrive as fixed denominations", t.dim_style()));
    }
    // Spending many small coins costs more fee than spending a few big ones, so say when it is
    // worth tidying — and which key does it.
    let small: u64 = held.iter().take(7).sum();
    if small >= 8 {
        tray.push(Span::styled(format!("  {small} small coins — A aggregates them"), Style::default().fg(t.attention)));
    }
    f.render_widget(Paragraph::new(vec![legend, bar, Line::from(stacks), Line::from(tray)]), inner);

    let wide = rest.width >= 110;
    let [coins_area, side] = if wide {
        Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(rest)
    } else {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(0)]).areas(rest)
    };
    let title = if app.jump_pending.is_some() { "coins · press a label" } else { "coins" };
    let block = panel(t, title, true);
    let inner = block.inner(coins_area);
    f.render_widget(block, coins_area);
    if s.coins.is_empty() {
        empty_state(
            f,
            inner,
            t,
            "◎",
            "No Qi yet. Coins arrive as fixed denominations, like cash.",
            &[("r", "receive"), ("C", "convert from QUAI")],
        );
    } else {
        let offset = app.view_offset();
        let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
        let rows: Vec<Row> = s
            .coins
            .iter()
            .enumerate()
            .skip(offset)
            .take(inner.height.saturating_sub(1) as usize)
            .map(|(i, c)| {
                let state = if c.reserved {
                    Span::styled("○ reserved", Style::default().fg(t.attention))
                } else if !c.unlock_height.is_zero() && U256::from(head) < c.unlock_height {
                    Span::styled("◕ locked", Style::default().fg(t.pending))
                } else {
                    Span::styled("✓", Style::default().fg(t.ok))
                };
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i - offset).to_string(), Style::default().fg(t.focus))),
                    Cell::from(Span::styled("◉", Style::default().fg(t.qi))),
                    Cell::from(
                        Line::from(Span::styled(amount::group_thousands(&amount::qi(U256::from(c.qits))), t.strong_style()))
                            .alignment(Alignment::Right),
                    ),
                    Cell::from(state),
                    Cell::from(c.label.clone().unwrap_or_else(|| c.origin.clone())),
                    Cell::from(Span::styled(short_address(&c.address), t.dim_style())),
                ]);
                if i == app.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        let table = Table::new(
            rows,
            [
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(12),
                Constraint::Length(10),
                Constraint::Min(12),
                Constraint::Length(13),
            ],
        )
        .header(Row::new(["", "", "Qi", "state", "origin", "address"]).style(t.dim_style()));
        f.render_widget(table, inner);
    }
    if side.width == 0 {
        return;
    }
    let [hist_area, addr_area] = Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(side);
    // Denomination histogram: where the cash sits.
    let values = wallet_core::sdk::consensus::Denomination::VALUES;
    let mut counts = vec![0u64; values.len()];
    for c in &s.coins {
        if let Some(n) = counts.get_mut(c.denomination as usize) {
            *n += 1;
        }
    }
    let max = counts.iter().copied().max().unwrap_or(1).max(1);
    let block = panel(t, "denominations", false);
    let inner = block.inner(hist_area);
    f.render_widget(block, hist_area);
    let bar_w = inner.width.saturating_sub(22) as u64;
    let lines: Vec<Line> = counts
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, n)| **n > 0)
        .take(inner.height as usize)
        .map(|(i, n)| {
            let cells = (n * bar_w * 8).div_ceil(max);
            let full = (cells / 8) as usize;
            let frac = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][(cells % 8) as usize];
            Line::from(vec![
                Span::styled(format!("{:>8} ", amount::qi(U256::from(values[i]))), t.dim_style()),
                Span::styled(format!("{}{frac}", "█".repeat(full)), Style::default().fg(t.qi)),
                Span::styled(format!(" ×{n}"), t.strong_style()),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
    let lines: Vec<Line> = app
        .dash
        .qi_addresses
        .iter()
        .rev()
        .take(addr_area.height.saturating_sub(2) as usize)
        .map(|(i, a, l)| {
            Line::from(vec![
                Span::styled(format!("#{i:<7} "), t.dim_style()),
                Span::raw(short_address(a)),
                Span::styled(format!(" {}", l.clone().unwrap_or_default()), Style::default().fg(t.qi)),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(panel(t, "receive & mining addresses", false)), addr_area);
}

pub(crate) fn draw_payments(f: &mut Frame, app: &App, t: &Theme, area: Rect, channels: bool) {
    let code = app.meta.as_ref().and_then(|m| m.payment_code.clone());
    let inner_w = area.width.saturating_sub(4).max(1) as usize;
    let code_rows = code.as_ref().map_or(1, |c| c.chars().count().div_ceil(inner_w));
    let [code_area, list_row] = Layout::vertical([Constraint::Length(code_rows as u16 + 3), Constraint::Min(6)]).areas(area);
    let lines = match code {
        Some(c) => vec![
            Line::from(Span::styled(c, t.strong_style().fg(t.qi))),
            Line::from(Span::styled(
                "Your payment code: share it to get paid privately. r QR · y copy on a row · d scan mailbox now",
                t.dim_style(),
            )),
        ],
        None => vec![Line::from(Span::styled("This wallet has no payment code (import a recovery phrase to get one).", t.dim_style()))],
    };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "you", false)), code_area);

    let wide = list_row.width >= 110;
    let [list_area, detail_area] = if wide {
        Layout::horizontal([Constraint::Min(50), Constraint::Length(48)]).areas(list_row)
    } else {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(0)]).areas(list_row)
    };
    let contacts_focused = !channels;
    let (contacts_area, channels_area) = if channels { (Rect::default(), list_area) } else { (list_area, Rect::default()) };
    let block = panel(t, &format!("contacts · {}", app.dash.contacts.len()), contacts_focused);
    let inner = block.inner(contacts_area);
    if !channels {
        f.render_widget(block, contacts_area);
    }
    if channels {
    } else if app.dash.contacts.is_empty() {
        empty_state(f, inner, t, "@", "No contacts yet. Save people by address, payment code, or both.", &[("a", "add contact")]);
    } else {
        let rows: Vec<Row> = app
            .dash
            .contacts
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let ledger = c
                    .address
                    .as_deref()
                    .and_then(|a| wallet_core::registry::parse_any_address(a).ok())
                    .map(|a| if a.ledger() == wallet_core::sdk::Ledger::Qi { ("Qi", t.qi) } else { ("QUAI", t.quai) });
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i).to_string(), Style::default().fg(t.focus))),
                    Cell::from(Span::styled(c.name.clone(), t.strong_style())),
                    Cell::from(match (&c.address, ledger) {
                        (Some(a), Some((label, color))) => {
                            Line::from(vec![Span::styled(format!("{label:<4} "), Style::default().fg(color)), Span::raw(short_address(a))])
                        }
                        _ => Line::from(Span::styled("—", t.dim_style())),
                    }),
                    Cell::from(match &c.payment_code {
                        Some(code) => Line::from(Span::styled(short_code(code), Style::default().fg(t.qi))),
                        None => Line::from(Span::styled("—", t.dim_style())),
                    }),
                    Cell::from(Span::styled(truncate(&c.note, 24), t.dim_style())),
                ]);
                if contacts_focused && i == app.selected { row.style(t.selected()) } else { row }
            })
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [Constraint::Length(1), Constraint::Length(16), Constraint::Length(19), Constraint::Length(16), Constraint::Min(8)],
            )
            .column_spacing(2)
            .header(Row::new(["", "name", "address", "payment code", "note"]).style(t.dim_style())),
            inner,
        );
    }
    if detail_area.width > 0 {
        let block = panel(t, "details", false);
        let inner = block.inner(detail_area);
        f.render_widget(block, detail_area);
        let mut lines: Vec<Line> = Vec::new();
        let label = |k: &str| Line::from(Span::styled(k.to_string(), t.dim_style()));
        let selected_contact = if !channels { app.dash.contacts.get(app.selected).cloned() } else { None };
        let selected_peer = if channels { app.channel_peer() } else { None };
        let selected_offer = if channels { app.channel_offer() } else { None };
        if let Some(c) = &selected_contact {
            lines.push(Line::from(Span::styled(c.name.clone(), t.strong_style().fg(t.focus))));
            lines.push(Line::from(""));
            if let Some(a) = &c.address {
                lines.push(label("address"));
                lines.push(Line::from(Span::styled(a.clone(), Style::default().fg(t.link))));
            }
            if let Some(code) = &c.payment_code {
                lines.push(label("payment code"));
                lines.push(Line::from(Span::styled(code.clone(), Style::default().fg(t.qi))));
                if let Some(p) = app.dash.peers.iter().find(|p| p.code == *code) {
                    lines.push(Line::from(Span::styled(
                        format!("channel · ↘ {} received · ↗ {} sent", p.receive_addresses, p.send_addresses),
                        t.dim_style(),
                    )));
                }
            }
            if !c.note.is_empty() {
                lines.push(label("note"));
                lines.push(Line::from(c.note.clone()));
            }
            // Token and NFT transfers with this person (explorer activity).
            if let Some(addr) = c.address.as_deref() {
                let with: Vec<&wallet_core::appdb::Activity> = app
                    .dash
                    .activity
                    .iter()
                    .filter(|a| a.detail["counterparty"].as_str().is_some_and(|cp| cp.eq_ignore_ascii_case(addr)))
                    .take(4)
                    .collect();
                if !with.is_empty() {
                    lines.push(label("transfers"));
                    for a in with {
                        let arrow = if a.direction == "in" { "↘" } else { "↗" };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{arrow} "), t.dim_style()),
                            Span::raw(super::views::activity_text(a)),
                        ]));
                    }
                }
            }
            lines.push(Line::from(""));
            let mut hints = vec![];
            if c.payment_code.is_some() || c.address.as_deref().is_some_and(|a| a.starts_with("0x")) {
                hints.push(("enter", "pay"));
            }
            if c.address.is_some() {
                hints.push(("Q", "send QUAI"));
            }
            if c.payment_code.is_some() {
                hints.push(("n", "notify"));
            }
            hints.extend([("e", "edit"), ("y", "copy"), ("x", "remove")]);
            for (k, v) in hints {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else if let Some(o) = selected_offer {
            lines.push(Line::from(Span::styled("channel offer", t.strong_style().fg(t.attention))));
            lines.push(Line::from(""));
            lines.push(label("payment code"));
            lines.push(Line::from(Span::styled(o.code.clone(), Style::default().fg(t.qi))));
            lines.push(Line::from(Span::styled(format!("{} Qi waiting (at least)", wallet_core::amount::qi(o.found)), t.dim_style())));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Announced through the mailbox. Anyone can announce, so it is not a channel until you accept it.",
                t.dim_style(),
            )));
            lines.push(Line::from(""));
            for (k, v) in [("enter", "accept"), ("x", "decline"), ("y", "copy")] {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else if let Some(p) = selected_peer {
            lines
                .push(Line::from(Span::styled(p.contact.clone().unwrap_or_else(|| "unsaved sender".into()), t.strong_style().fg(t.focus))));
            lines.push(Line::from(""));
            lines.push(label("payment code"));
            lines.push(Line::from(Span::styled(p.code.clone(), Style::default().fg(t.qi))));
            lines
                .push(Line::from(Span::styled(format!("↘ {} received · ↗ {} sent", p.receive_addresses, p.send_addresses), t.dim_style())));
            lines.push(Line::from(""));
            let save = if p.contact.is_some() { "edit contact" } else { "save as contact" };
            for (k, v) in [("enter", "pay Qi"), ("a", save), ("S", "rescan"), ("n", "notify"), ("y", "copy")] {
                lines.push(Line::from(vec![Span::styled(format!("{k:>5}  "), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
        } else {
            lines.push(Line::from(Span::styled("select a contact or channel", t.dim_style())));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }

    if !channels {
        return;
    }
    let channels_focused = true;
    let offered = if app.dash.offers.is_empty() { String::new() } else { format!(" · {} offered", app.dash.offers.len()) };
    let block = panel(t, &format!("payment channels · {}{offered}", app.dash.peers.len()), channels_focused);
    let inner = block.inner(channels_area);
    f.render_widget(block, channels_area);
    if !app.dash.unlocked {
        empty_state(f, inner, t, "○", "Unlock to see payment channels.", &[]);
    } else if app.dash.peers.is_empty() && app.dash.offers.is_empty() {
        empty_state(
            f,
            inner,
            t,
            "@",
            "No channels yet. A sender who announces a channel and pays you is offered here, to accept or decline.",
            &[("d", "scan mailbox"), ("p", "add a peer")],
        );
    } else {
        let offers = app.dash.offers.len();
        let offer_rows = app.dash.offers.iter().enumerate().map(|(i, o)| {
            let row = Row::new(vec![
                Cell::from(Span::styled(app::jump_label(i).to_string(), Style::default().fg(t.focus))),
                Cell::from(Span::styled(short_code(&o.code), Style::default().fg(t.qi))),
                Cell::from(Span::styled("offered · enter accepts, x declines", Style::default().fg(t.attention))),
                Cell::from(format!("{} Qi", wallet_core::amount::qi(o.found))),
                Cell::from(""),
            ]);
            if channels_focused && i == app.selected { row.style(t.selected()) } else { row }
        });
        let rows: Vec<Row> = offer_rows
            .chain(app.dash.peers.iter().enumerate().map(|(i, p)| (i + offers, p)).map(|(i, p)| {
                let who = match &p.contact {
                    Some(name) => Span::styled(name.clone(), t.strong_style()),
                    None => Span::styled("unsaved · a to save", Style::default().fg(t.attention)),
                };
                let row = Row::new(vec![
                    Cell::from(Span::styled(app::jump_label(i).to_string(), Style::default().fg(t.focus))),
                    Cell::from(Span::styled(short_code(&p.code), Style::default().fg(t.qi))),
                    Cell::from(who),
                    Cell::from(format!("↘ {}", p.receive_addresses)),
                    Cell::from(format!("↗ {}", p.send_addresses)),
                ]);
                if channels_focused && i == app.selected { row.style(t.selected()) } else { row }
            }))
            .collect();
        f.render_widget(
            Table::new(
                rows,
                [Constraint::Length(1), Constraint::Length(16), Constraint::Min(20), Constraint::Length(10), Constraint::Length(8)],
            )
            .column_spacing(2)
            .header(Row::new(["", "payment code", "contact", "recv", "sent"]).style(t.dim_style())),
            inner,
        );
    }
}

pub(crate) fn draw_locks(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let locks_area = area;
    let agrees = match &app.eco.lockups {
        Some(Ok(total)) if (*total == 0) == app.dash.locks.iter().all(|l| l.unlocked) => " · ✓ explorer agrees",
        Some(Ok(_)) => " · ! explorer differs",
        _ => "",
    };
    let block = panel(t, &format!("time locks{agrees}"), true);
    let inner = block.inner(locks_area);
    f.render_widget(block, locks_area);
    if app.dash.locks.is_empty() {
        empty_state(
            f,
            inner,
            t,
            "◕",
            "Nothing time-locked: everything is spendable. Converted coins wait here, with a countdown, until they unlock.",
            &[("2 ]]]", "convert")],
        );
    }
    // Exact countdowns; the lock start height isn't known for every source.
    for (i, l) in app.dash.locks.iter().enumerate().take(inner.height as usize) {
        let row = Rect::new(inner.x, inner.y + i as u16, inner.width, 1);
        let when = if l.unlocked {
            Span::styled("✓ spendable", Style::default().fg(t.ok))
        } else {
            let at = l
                .unlock_height
                .map(|h| format!("◕ unlocks #{}", amount::group_thousands(&h.to_string())))
                .unwrap_or_else(|| "○ settling".into());
            let remaining = l.blocks_remaining.map(|b| format!(" · {b} blocks")).unwrap_or_default();
            let eta = l.eta_secs.map(|s| format!(" · ~{}", human_duration(s))).unwrap_or_default();
            Span::styled(format!("{at}{remaining}{eta}"), Style::default().fg(t.pending))
        };
        let style = if i == app.selected { t.selected() } else { t.text_style() };
        let color = if l.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
        let line = Line::from(vec![
            Span::styled("▌", Style::default().fg(color)),
            Span::styled(format!("{:>18} {:<5} ", l.amount, l.asset), style.add_modifier(Modifier::BOLD)),
            when,
            Span::styled(format!("  {}", l.source), t.dim_style()),
        ]);
        f.render_widget(Paragraph::new(line).style(style), row);
    }
}

/// Right-aligned samples scaled between their min and max so small changes stay visible.
pub(crate) fn scaled(data: &[u64], width: u16) -> Vec<u64> {
    let tail: Vec<u64> = data.iter().rev().take(width as usize).rev().copied().collect();
    let min = tail.iter().copied().min().unwrap_or(0);
    let mut out = vec![0u64; (width as usize).saturating_sub(tail.len())];
    out.extend(tail.iter().map(|v| v - min + 1));
    out
}

/// A series resampled to exactly `width` bars, so a 48-hour history fills its panel rather than
/// sitting in one corner of it. Bars are measured up from a little under the lowest value — the
/// shape is what a sparkline is for, and from zero every hour of a busy chain looks the same.
pub(crate) fn stretched(values: &[f64], width: u16) -> Vec<u64> {
    let width = width as usize;
    if values.is_empty() || width == 0 {
        return vec![0; width];
    }
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let floor = min - (max - min).max(min.abs() * 0.05) * 0.25;
    let span = (max - floor).max(f64::MIN_POSITIVE);
    (0..width)
        .map(|x| {
            let v = values[(x * values.len() / width).min(values.len() - 1)];
            (((v - floor) / span) * 100.0).round().max(1.0) as u64
        })
        .collect()
}

/// `100.7M`, `7,651`: counts on a chart title, short enough to leave room for the chart.
fn count_text(n: u64) -> String {
    match n {
        n if n >= 1_000_000_000 => format!("{:.2}B", n as f64 / 1e9),
        n if n >= 10_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n => amount::group_thousands(&n.to_string()),
    }
}

/// `38,567 gwei`, from gwei.
fn gwei_text(gwei: f64) -> String {
    format!("{} gwei", amount::group_thousands(&format!("{:.0}", gwei.max(0.0))))
}

/// The chain as the explorer sees it, beside the node's own health: totals, the last hour, and
/// where the figures came from and how old they are.
fn draw_chain(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let kv = |k: &str, v: Vec<Span<'static>>| {
        let mut spans = vec![Span::styled(format!("{k:<14}"), t.dim_style())];
        spans.extend(v);
        Line::from(spans)
    };
    let lines = match &app.eco.chain_stats {
        None => vec![Line::from(Span::styled(format!("{} reading network statistics…", spinner()), t.dim_style()))],
        Some(Err(e)) => vec![
            Line::from(Span::styled(format!("○ {}", truncate(&app::friendly_error(e), 80)), t.dim_style())),
            Line::from(Span::styled("Node health on the left is read from the node itself.", t.dim_style())),
        ],
        Some(Ok(s)) => {
            let last = s.hours.last();
            let mut lines = vec![
                kv("avg block", vec![Span::raw(s.avg_block_secs.map(|b| format!("{b:.2} s")).unwrap_or_else(|| "—".into()))]),
                kv(
                    "transactions",
                    vec![
                        Span::styled(
                            s.total_transactions.map(|n| amount::group_thousands(&n.to_string())).unwrap_or_else(|| "—".into()),
                            t.strong_style(),
                        ),
                        Span::styled(
                            last.map(|h| format!(" · {} last hour", amount::group_thousands(&h.transactions.to_string())))
                                .unwrap_or_default(),
                            t.dim_style(),
                        ),
                    ],
                ),
            ];
            if let Some(h) = last {
                let cross = h.transactions.saturating_sub(h.quai_transactions + h.qi_transactions);
                lines.push(kv(
                    "",
                    vec![Span::styled(
                        format!(
                            "{} QUAI · {} Qi · {} cross-zone",
                            amount::group_thousands(&h.quai_transactions.to_string()),
                            amount::group_thousands(&h.qi_transactions.to_string()),
                            amount::group_thousands(&cross.to_string())
                        ),
                        t.dim_style(),
                    )],
                ));
            }
            let addresses = match (s.quai_addresses, s.qi_addresses) {
                (Some(q), Some(i)) => {
                    format!("{} QUAI · {} Qi", amount::group_thousands(&q.to_string()), amount::group_thousands(&i.to_string()))
                }
                (Some(q), None) => amount::group_thousands(&q.to_string()),
                _ => "—".into(),
            };
            lines.push(kv("addresses", vec![Span::raw(addresses)]));
            lines.push(kv(
                "block reward",
                vec![Span::raw(s.block_reward_quai.map(|r| format!("{r:.2} QUAI")).unwrap_or_else(|| "—".into()))],
            ));
            if let Some(g) = last.and_then(|h| h.avg_gas_price_gwei()) {
                lines.push(kv("gas paid", vec![Span::raw(gwei_text(g)), Span::styled(" avg, last hour", t.dim_style())]));
            }
            let observed = s.observed_at.min(wallet_core::registry::now());
            let when = match ago(observed) {
                now if now == "now" => "just now".to_string(),
                age => format!("as of {age} ago"),
            };
            lines.push(kv("source", vec![Span::styled(format!("explorer.qu.ai · {when}"), t.dim_style())]));
            lines
        }
    };
    f.render_widget(Paragraph::new(lines).block(panel(t, "chain", false)), area);
}

/// Hashrate, transactions and gas over time: three panels on one row.
fn draw_chain_charts(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let [hash, txs, gas] =
        Layout::horizontal([Constraint::Percentage(36), Constraint::Percentage(32), Constraint::Percentage(32)]).areas(area);
    let stats = app.eco.chain_stats.as_ref().and_then(|r| r.as_ref().ok());
    // Hashrate: one row per algorithm, each on its own scale — they differ by six orders of
    // magnitude, so one shared axis would draw two flat lines and a wall.
    let block = panel(t, "hashrate · 24h", false);
    let inner = block.inner(hash);
    f.render_widget(block, hash);
    match stats {
        Some(s) if !s.hashrate_history.is_empty() || s.hashrate.sha > 0.0 => {
            type Pick = fn(&wallet_core::chainstats::Hashrates) -> f64;
            let algos: [(&str, Pick, Color); 3] =
                [("SHA", |h| h.sha, t.focus), ("Scrypt", |h| h.scrypt, t.ok), ("KawPoW", |h| h.kawpow, t.attention)];
            for (i, (name, pick, colour)) in algos.iter().enumerate() {
                let y = inner.y + (i as u16) * 2;
                if y >= inner.bottom() {
                    break;
                }
                let label = format!("{name:<7}{:>11} ", wallet_core::chainstats::hashrate_text(pick(&s.hashrate)));
                let label_w = (label.chars().count() as u16).min(inner.width);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(format!("{name:<7}"), t.dim_style()),
                        Span::styled(label[7..].to_string(), t.strong_style()),
                    ])),
                    Rect { x: inner.x, y, width: label_w, height: 1 },
                );
                let spark = Rect { x: inner.x + label_w, y, width: inner.width.saturating_sub(label_w), height: 1 };
                let series: Vec<f64> = s.hashrate_history.iter().map(|(_, h)| pick(h)).collect();
                f.render_widget(Sparkline::default().data(stretched(&series, spark.width)).style(Style::default().fg(*colour)), spark);
            }
        }
        _ => chart_placeholder(f, app, t, inner),
    }
    // Transactions per hour, with the running total in the title.
    let total = stats.and_then(|s| s.total_transactions).map(|n| format!(" · total {}", count_text(n))).unwrap_or_default();
    let last = stats.and_then(|s| s.hours.last()).map(|h| format!(" · {}/h", count_text(h.transactions))).unwrap_or_default();
    let block = panel(t, &format!("transactions{last}{total}"), false);
    let inner = block.inner(txs);
    f.render_widget(block, txs);
    match stats.filter(|s| !s.hours.is_empty()) {
        Some(s) => {
            let series: Vec<f64> = s.hours.iter().map(|h| h.transactions as f64).collect();
            spark_with_axis(f, app, t, inner, &series, t.ok, s.hours.len());
        }
        None => chart_placeholder(f, app, t, inner),
    }
    // Gas: the node's price now in the title, what people actually paid each hour below.
    let node = app.dash.health.as_ref().and_then(|h| h.gas_price.parse::<f64>().ok()).filter(|p| *p > 0.0).map(|wei| wei / 1e9);
    let title = match node {
        Some(g) => format!("gas · node {} now", gwei_text(g)),
        None => "gas · paid per hour".into(),
    };
    let block = panel(t, &title, false);
    let inner = block.inner(gas);
    f.render_widget(block, gas);
    let paid: Vec<f64> = stats.map(|s| s.hours.iter().filter_map(|h| h.avg_gas_price_gwei()).collect()).unwrap_or_default();
    if paid.is_empty() {
        chart_placeholder(f, app, t, inner);
    } else {
        spark_with_axis(f, app, t, inner, &paid, t.attention, paid.len());
    }
}

/// A sparkline over all but the last row, and under it how far back it reaches and its range.
fn spark_with_axis(f: &mut Frame, app: &App, t: &Theme, inner: Rect, series: &[f64], colour: Color, hours: usize) {
    if inner.height < 2 || inner.width < 8 {
        return;
    }
    let chart = Rect { height: inner.height - 1, ..inner };
    f.render_widget(Sparkline::default().data(stretched(series, chart.width)).style(Style::default().fg(colour)), chart);
    super::edge::ramp_bars(app, f.buffer_mut(), chart, colour, t);
    let (lo, hi) = series.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| (a.min(*v), b.max(*v)));
    let range = format!("{} – {}", count_text(lo.max(0.0) as u64), count_text(hi.max(0.0) as u64));
    let left = format!("{hours}h ago");
    let gap = (inner.width as usize).saturating_sub(left.len() + range.len() + 4);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(left, t.dim_style()),
            Span::styled(format!("{:gap$}", ""), t.dim_style()),
            Span::styled(range, t.dim_style()),
            Span::styled("  now", t.dim_style()),
        ])),
        Rect { y: inner.bottom() - 1, height: 1, ..inner },
    );
}

/// What a chart shows before its data arrives, or when there is none for this network.
fn chart_placeholder(f: &mut Frame, app: &App, t: &Theme, inner: Rect) {
    let text = match &app.eco.chain_stats {
        None => format!("{} loading…", spinner()),
        Some(Err(_)) => "○ no statistics for this network".into(),
        Some(Ok(_)) => "○ no history yet".into(),
    };
    f.render_widget(Paragraph::new(Span::styled(text, t.dim_style())), inner);
}

pub(crate) fn draw_node(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // Node health beside the chain's own figures, the node's two sparklines, the chain's history,
    // then the network list. The history row is dropped before the list when the terminal is short.
    let history = if area.height >= 30 { 7 } else { 0 };
    let [top, charts, chain_charts, nets] =
        Layout::vertical([Constraint::Length(10), Constraint::Length(5), Constraint::Length(history), Constraint::Min(4)]).areas(area);
    let (info, chain) = if top.width >= 110 {
        let [a, b] = Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(top);
        (a, Some(b))
    } else {
        (top, None)
    };
    if let Some(chain) = chain {
        draw_chain(f, app, t, chain);
    }
    if history > 0 {
        draw_chain_charts(f, app, t, chain_charts);
    }
    let mut lines = Vec::new();
    let kv = |k: &str, v: String| Line::from(vec![Span::styled(format!("{k:<14}"), t.dim_style()), Span::raw(v)]);
    match (&app.dash.health, &app.dash.node_error) {
        (_, Some(e)) => {
            lines.push(Line::from(Span::styled(format!("× {}", app::friendly_error(e)), Style::default().fg(t.danger))));
            lines.push(Line::from(Span::styled(
                "Retrying automatically. Check the RPC URL with `quai-terminal network list`.",
                t.dim_style(),
            )));
        }
        (Some(h), None) => {
            lines.push(kv("network", format!("{} ({})", app.dash.network_name, app.dash.network_id)));
            lines.push(Line::from(vec![
                Span::styled(format!("{:<14}", "identity"), t.dim_style()),
                Span::styled(
                    if h.identity_ok { "✓ chain id and genesis match" } else { "× MISMATCH — do not transact" },
                    Style::default().fg(if h.identity_ok { t.ok } else { t.danger }),
                ),
            ]));
            lines.push(kv("chain id", h.chain_id.clone()));
            lines.push(kv("genesis", h.genesis.clone()));
            lines.push(kv("height", amount::group_thousands(&h.height.to_string())));
            lines.push(kv(
                "head age",
                h.head_age_secs.map(|s| if s < 60 { format!("{s}s") } else { human_duration(s) }).unwrap_or_else(|| "?".into()),
            ));
            lines.push(kv(
                "gas price",
                format!("{} gwei", amount::group_thousands(&amount::format_amount_short(h.gas_price.parse().unwrap_or_default(), 9, 3))),
            ));
            lines.push(kv("client", h.client_version.clone().unwrap_or_else(|| "—".into())));
            lines.push(kv(
                "monitoring",
                match app.config.monitor_endpoints.get(&app.dash.network_id) {
                    Some(m) => format!("{} · reads only (m to change)", m.rpc_url),
                    None => "main RPC (m to add a read-only endpoint)".into(),
                },
            ));
        }
        _ => lines.push(Line::from(Span::styled(format!("{} checking node…", spinner()), t.dim_style()))),
    }
    f.render_widget(Paragraph::new(lines).block(panel(t, "node health", false)), info);
    let [lat, blocks] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(charts);
    let hist = &app.dash.latency_history;
    let (min, max, last) =
        (hist.iter().min().copied().unwrap_or(0), hist.iter().max().copied().unwrap_or(0), hist.last().copied().unwrap_or(0));
    let block = panel(t, &format!("latency {last} ms · min {min} · max {max}"), false);
    let inner = block.inner(lat);
    f.render_widget(block, lat);
    f.render_widget(Sparkline::default().data(scaled(hist, inner.width)).style(Style::default().fg(t.focus)), inner);
    super::edge::ramp_bars(app, f.buffer_mut(), inner, t.focus, t);
    let deltas: Vec<u64> = app.dash.height_history.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
    let block = panel(t, &format!("blocks per refresh · last {}", deltas.last().copied().unwrap_or(0)), false);
    let inner = block.inner(blocks);
    f.render_widget(block, blocks);
    f.render_widget(Sparkline::default().data(scaled(&deltas, inner.width)).style(Style::default().fg(t.ok)), inner);
    super::edge::ramp_bars(app, f.buffer_mut(), inner, t.ok, t);
    let rows: Vec<Row> = app
        .dash
        .networks
        .iter()
        .enumerate()
        .map(|(i, (id, name))| {
            let active = *id == app.dash.network_id;
            let row = Row::new(vec![
                Cell::from(Span::styled(if active { "●" } else { "○" }, Style::default().fg(if active { t.ok } else { t.dim }))),
                Cell::from(id.clone()),
                Cell::from(name.clone()),
                Cell::from(Span::styled(if id == "mainnet" { "real funds" } else { "" }, Style::default().fg(t.attention))),
            ]);
            if i == app.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let block = panel(t, "networks · enter to switch", true);
    let inner = block.inner(nets);
    f.render_widget(block, nets);
    f.render_widget(Table::new(rows, [Constraint::Length(1), Constraint::Length(16), Constraint::Min(10), Constraint::Length(12)]), inner);
}

/// A feature switch: what it covers while on.
fn feature_value(on: bool, covers: &str) -> String {
    if on { format!("● on · {covers}") } else { "○ off".into() }
}

pub(crate) fn draw_settings(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let c = &app.config;
    let on_off = |b: bool| if b { "● on".to_string() } else { "○ off".to_string() };
    let rows: Vec<Row> = SETTINGS
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let value = match *id {
                "theme" => {
                    format!("{}  ›", super::themes::find(&c.theme).map(|e| e.name.to_string()).unwrap_or_else(|| app.theme.name.clone()))
                }
                "motion" => format!("{:?}{}", c.motion, if app.motion() != c.motion { " (reduced over SSH)" } else { "" }).to_lowercase(),
                "daemon" => {
                    let running = match crate::daemon::state(&app.paths) {
                        Some(s) => format!("running · {} wallets, {} unlocked", s.wallets.len(), s.wallets.iter().filter(|w| w.2).count()),
                        None => "not running".into(),
                    };
                    format!("{} · {running}", if c.daemon_autostart { "● starts with the terminal" } else { "○ off" })
                }
                "daemon_unlock" => {
                    if c.daemon_share_unlock {
                        "● each unlock here unlocks it in the daemon".into()
                    } else {
                        "○ off".into()
                    }
                }
                "layout" => match c.layout.as_str() {
                    "trader" => "trader · Markets beside the swap card".into(),
                    "focus" => "focus · no sidebar".into(),
                    "standard" => "standard".into(),
                    _ => "auto · trader from 200 columns".into(),
                },
                "feature:messaging" => feature_value(c.features.messaging, "board, sealed DMs, chat dock"),
                "feature:trading" => feature_value(c.features.trading, "markets, swap, pools, launches"),
                "feature:nfts" => feature_value(c.features.nfts, "collected, explore, listings"),
                "ceremonies" => on_off(c.ceremonies),
                "sound" => on_off(c.sound),
                "big_numbers" => on_off(c.big_numbers),
                "balance_in_bar" => format!("{} · $ toggles it", on_off(c.balance_in_bar)),
                "lock_effect" => format!("{}  ›", c.lock_effect),
                "notifications" => on_off(c.notifications),
                "autolock" => {
                    if c.auto_lock_minutes == 0 {
                        "○ off".into()
                    } else {
                        format!("{} min", c.auto_lock_minutes)
                    }
                }
                "prices" => on_off(c.fetch_prices),
                "ipfs" | "abi_ipfs" => {
                    let content = if *id == "abi_ipfs" { wallet_core::ipfs::Content::Abi } else { wallet_core::ipfs::Content::Media };
                    let g = wallet_core::ipfs::gateway(content);
                    let kind = if g.is_local() {
                        "your node"
                    } else if g.is_default_for(content) {
                        "default"
                    } else {
                        "custom"
                    };
                    format!("{} · {kind}  ›", g.display())
                }
                _ => "›".into(),
            };
            let row = Row::new(vec![Cell::from(*label), Cell::from(Span::styled(value, Style::default().fg(t.focus)))]);
            if i == app.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let [list, info] = Layout::vertical([Constraint::Length(SETTINGS.len() as u16 + 2), Constraint::Min(4)]).areas(area);
    let block = panel(t, "settings", true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    // A short terminal cannot show every row: the list scrolls with the cursor.
    let visible = (inner.height as usize).max(1);
    let offset = app.selected.saturating_sub(visible - 1);
    let rows: Vec<Row> = rows.into_iter().skip(offset).take(visible).collect();
    f.render_widget(Table::new(rows, [Constraint::Length(30), Constraint::Min(20)]), inner);
    let caps = &app.caps;
    let lines = vec![
        Line::from(vec![
            Span::styled("terminal      ", t.dim_style()),
            Span::raw({
                let tier = match caps.tier {
                    Tier::Pixels => "pixel QR codes",
                    Tier::Cells => "block QR codes",
                    Tier::Text => "no QR codes",
                };
                let mut parts =
                    vec![caps.terminal.clone(), tier.to_string(), if caps.truecolor { "24-bit color".into() } else { "256 colors".into() }];
                if caps.tmux {
                    parts.push("inside tmux".into());
                }
                if caps.ssh {
                    parts.push("over SSH (reduced motion)".into());
                }
                parts.join(" · ")
            }),
        ]),
        Line::from(vec![
            Span::styled("theme         ", t.dim_style()),
            Span::raw(format!("{} · {}", app.theme.name, app::short_path(&app.theme.source))),
            Span::styled(app.theme.text_contrast().map(|c| format!("  · text contrast {c:.1}:1")).unwrap_or_default(), t.dim_style()),
        ]),
        Line::from(vec![
            Span::styled("data          ", t.dim_style()),
            Span::raw(app::short_path(&app.paths.root().display().to_string())),
        ]),
        Line::from(""),
        Line::from(Span::styled("With theme = auto the wallet follows Omarchy live; T opens the showroom from anywhere.", t.dim_style())),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "environment", false)), info);
}

// ---------------------------------------------------------------- lock / onboarding

/// How long the outgoing effect's last frame takes to dissolve under the incoming one.
const HANDOVER_MS: u128 = 500;

fn draw_lock(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    // The effect canvas is built for exactly this art area (see `app::lock_art_size`).
    let (_, art_h) = app::lock_art_size((area.width, area.height));
    // `centered` keeps a row of margin above and below, so the band has to be two taller than
    // the card itself — at 9 the card lost its last line, which is the line that speaks.
    let [_, art, form, _] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(art_h), Constraint::Length(11), Constraint::Min(0)]).areas(area);
    let accent = t.accent_rgb.map(|(r, g, b)| Color::Rgb(r, g, b)).unwrap_or(t.focus);
    // One effect rolls straight into the next: the moment a ceremony runs out it is replaced and
    // stepped on this same frame, so the screen never drops to a still wordmark between them.
    // Stepping the new one here matters — a freshly built ceremony has no frame yet, and painting
    // it before its first step would blank the canvas for one redraw at every hand-over.
    if app.ambient.as_mut().is_none_or(|c| !c.advance()) {
        // Keep the wordmark the finished effect left on screen; it dissolves under the new one
        // below, so the hand-over is a cross-fade rather than a cut to an empty canvas.
        app.lock_fade = app.ambient.as_ref().and_then(|c| c.frame()).map(|f| (f.to_string(), std::time::Instant::now()));
        app.ambient = None;
        app.start_lock_ceremony((area.width, area.height));
        if let Some(c) = app.ambient.as_mut() {
            c.step();
        }
    }
    // Under the incoming effect, never over it: an effect paints opaque cells, and a half-gone
    // wordmark drawn on top would punch holes in whatever is arriving.
    if let Some((frame, at)) = app.lock_fade.take() {
        let ms = at.elapsed().as_millis();
        if ms < HANDOVER_MS && app.ambient.is_some() {
            let keep = 1.0 - ms as f32 / HANDOVER_MS as f32;
            super::fx::paint_dissolve(&frame, art, f.buffer_mut(), t.base().fg(accent), keep);
            app.lock_fade = Some((frame, at));
        }
    }
    if let Some(c) = &app.ambient {
        c.render(art, f.buffer_mut(), t.base().fg(accent));
    } else {
        // Effects off (reduced motion, plain mode): the wordmark rests, with the chain line under it.
        wordmark(f, art, t);
        if let Some(line) = app.chain_weather() {
            f.render_widget(
                Paragraph::new(Span::styled(line, t.dim_style())).alignment(Alignment::Center),
                Rect { y: art.bottom().saturating_sub(1), height: 1, ..art },
            );
        }
    }
    let rect = centered(form, 58, 8);
    let inner = modal_frame(f, rect, t, "locked");
    let name = app.meta.as_ref().map(|m| m.name.clone()).unwrap_or_default();
    let dots = "•".repeat(app.lock_input.chars().count().min(40));
    let lines = vec![
        Line::from(vec![
            super::images::native_span(app, t, "quai"),
            Span::raw(" "),
            Span::styled(name, t.strong_style()),
            Span::styled(format!("  ·  {}", app.network_id), t.dim_style()),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled("password  ", t.dim_style()), Span::styled(format!("{dots}▏"), Style::default().fg(t.focus))]),
        Line::from(Span::styled(
            "▔".repeat(inner.width.saturating_sub(10) as usize),
            if app.lock_error.is_some() { Style::default().fg(t.danger) } else { t.dim_style() },
        ))
        .alignment(Alignment::Right),
        // What this screen is doing, in its own words: the unlock it was asked for beats any
        // background work, and an answer that came back beats the hint.
        Line::from(match (app.unlocking, &app.lock_error) {
            (true, _) => Span::styled(format!("{} unlocking…", spinner()), Style::default().fg(t.pending)),
            (false, Some(e)) => Span::styled(format!("× {e}"), Style::default().fg(t.danger)),
            (false, None) => Span::styled("enter unlock · esc clear · ctrl-c quit", t.dim_style()),
        }),
    ];
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
}

pub(crate) fn wordmark(f: &mut Frame, area: Rect, t: &Theme) {
    let block = super::fx::wordmark_block();
    let lines: Vec<Line> = block
        .lines()
        .enumerate()
        .map(|(i, l)| {
            Line::from(Span::styled(l.to_string(), Style::default().fg(if i < 2 { t.quai } else { t.qi }).add_modifier(Modifier::BOLD)))
        })
        .collect();
    let h = lines.len() as u16;
    let rect = Rect::new(area.x, area.y + area.height.saturating_sub(h) / 2, area.width, h.min(area.height));
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), rect);
}

fn steps_line(t: &Theme, current: usize) -> Line<'static> {
    let names = ["look", "privacy", "connections", "wallet", "protect"];
    let mut spans = Vec::new();
    for (i, n) in names.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" ── ", t.dim_style()));
        }
        let step = i + 1;
        let style = if step == current {
            t.strong_style().fg(t.focus)
        } else if step < current {
            Style::default().fg(t.ok)
        } else {
            t.dim_style()
        };
        let mark = if step < current { "✓".to_string() } else { step.to_string() };
        spans.push(Span::styled(format!("{mark} {n}"), style));
    }
    Line::from(spans)
}

fn draw_onboarding(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let Some(ob) = app.onboarding.as_ref() else { return };
    let step = super::onboarding::step(ob);
    let [top, body, bottom] = Layout::vertical([Constraint::Length(2), Constraint::Min(10), Constraint::Length(1)]).areas(area);
    f.render_widget(Paragraph::new(vec![Line::from(""), steps_line(t, step)]).alignment(Alignment::Center), top);
    let hint: String = match ob {
        Onboarding::Theme(_) => "↑↓ preview · type to filter · enter choose · esc keep current".into(),
        Onboarding::Privacy { .. } => "↑↓ choose · enter continue · esc back to looks · change any time in System › Data sources".into(),
        Onboarding::Connections { .. } => {
            "tab/↓ next · enter continue (empty keeps the default) · ctrl-u clear · esc back · change any time in Settings".into()
        }
        Onboarding::Choose { .. } => "↑↓ choose · enter continue · n network · t themes · ctrl-c quit".into(),
        Onboarding::ShowPhrase { .. } => "enter I wrote it down · esc start over".into(),
        Onboarding::Quiz { .. } => "tab next word · enter check · esc show the phrase again · ctrl-s skip (not recommended)".into(),
        Onboarding::Details { .. } => "tab/↓ next field · enter continue · ctrl-u clear · esc back".into(),
    };
    f.render_widget(Paragraph::new(Span::styled(hint, t.dim_style())).alignment(Alignment::Center), bottom);
    match ob {
        Onboarding::Theme(picker) => {
            let rect = centered(body, 112, 30);
            let inner = modal_frame(f, rect, t, "pick a look · you can change it any time with T");
            draw_showroom(f, inner, t, picker);
        }
        Onboarding::Privacy { selected } => {
            let rect = centered(body, 90, 14);
            let inner = modal_frame(f, rect, t, "privacy");
            let mut lines = vec![
                Line::from(Span::styled("Who should learn which addresses are yours?", t.strong_style())),
                Line::from(Span::styled(
                    "Your node always sees the addresses it is asked about. Run your own, and nobody else does.",
                    t.dim_style(),
                )),
                Line::from(""),
            ];
            for (i, (label, sub, _)) in super::onboarding::PRIVACY.iter().enumerate() {
                let active = i == *selected;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{label:<11}"), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                    Span::styled(*sub, t.dim_style()),
                ]));
                lines.push(Line::from(""));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Connections { fields, focus } => {
            let rect = centered(body, 96, 22);
            let inner = modal_frame(f, rect, t, "connections · every one of these has a default that works");
            let mut lines = vec![
                Line::from(Span::styled("Where this wallet reads from.", t.strong_style())),
                Line::from(Span::styled("Press enter through them all to take the defaults.", t.dim_style())),
                Line::from(""),
            ];
            for (i, field) in fields.iter().enumerate() {
                let active = i == *focus;
                let shown = if field.value.is_empty() { field.hint.clone() } else { field.value.clone() };
                let style = if field.value.is_empty() { t.dim_style() } else { t.text_style() };
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{:<16}", field.label), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                    Span::styled(shown, style),
                    Span::styled(if active { "▏" } else { "" }, Style::default().fg(t.focus)),
                ]));
            }
            lines.push(Line::from(""));
            // Why the field under the cursor is worth setting, in front of the person deciding.
            if let Some((_, why)) = super::onboarding::CONNECTIONS.get(*focus) {
                lines.push(Line::from(Span::styled(*why, t.dim_style())));
            }
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Choose { selected } => {
            let rect = centered(body, 86, 18);
            let inner = modal_frame(f, rect, t, "welcome");
            let [mark, rest] = Layout::vertical([Constraint::Length(5), Constraint::Min(4)]).areas(inner);
            wordmark(f, mark, t);
            let mut lines = vec![
                Line::from(vec![
                    Span::raw("A self-custodial wallet for "),
                    Span::styled("QUAI", t.strong_style().fg(t.quai)),
                    Span::raw(" and "),
                    Span::styled("Qi", t.strong_style().fg(t.qi)),
                    Span::raw(", right in your terminal."),
                ]),
                Line::from(""),
            ];
            for (i, (label, sub)) in super::onboarding::CHOICES.iter().enumerate() {
                let active = i == *selected;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{label:<28}"), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
                    Span::styled(*sub, t.dim_style()),
                ]));
            }
            lines.push(Line::from(""));
            let mainnet = app.network_id == "mainnet";
            lines.push(Line::from(vec![
                Span::styled("network  ", t.dim_style()),
                Span::styled(app.network_id.clone(), if mainnet { Style::default().fg(t.link) } else { t.strong_style().fg(t.attention) }),
                Span::styled(if mainnet { "  · real funds · n to change" } else { "  · n to change" }, t.dim_style()),
            ]));
            lines.push(Line::from(vec![
                Span::styled("stored   ", t.dim_style()),
                Span::styled(app::short_path(&app.paths.root().display().to_string()), t.dim_style()),
            ]));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), rest);
        }
        Onboarding::ShowPhrase { phrase } => {
            let rect = centered(body, 86, phrase.split_whitespace().count().div_ceil(4) as u16 + 10);
            let inner = modal_frame(f, rect, t, "your recovery phrase");
            let mut lines = vec![
                Line::from(Span::styled("Write these 24 words down, in order, on paper.", t.strong_style())),
                Line::from(Span::styled(
                    "Anyone who has them controls your funds. Never type them into a website or share a photo.",
                    Style::default().fg(t.attention),
                )),
                Line::from(""),
            ];
            let words: Vec<&str> = phrase.split_whitespace().collect();
            let rows = words.len().div_ceil(4);
            for r in 0..rows {
                let mut spans = Vec::new();
                for c in 0..4 {
                    // Column-first numbering: 1–6 down the first column.
                    if let Some(w) = words.get(c * rows + r) {
                        spans.push(Span::styled(format!("{:>4} ", c * rows + r + 1), t.dim_style()));
                        spans.push(Span::styled(format!("{w:<14}"), t.strong_style()));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("Next you'll confirm three of the words.", t.dim_style())));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Quiz { indexes, answers, focus, .. } => {
            let rect = centered(body, 70, 12);
            let inner = modal_frame(f, rect, t, "confirm your backup");
            let mut lines = vec![Line::from(Span::styled("Type these words from your written copy:", t.text_style())), Line::from("")];
            for i in 0..3 {
                let active = i == *focus;
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                    Span::styled(format!("word #{:<3}  ", indexes[i] + 1), t.dim_style()),
                    Span::styled(
                        format!("{}{}", answers[i], if active { "▏" } else { "" }),
                        if active { t.strong_style().fg(t.focus) } else { t.text_style() },
                    ),
                ]));
            }
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Onboarding::Details { kind, fields, focus, verified, .. } => {
            let heading = match kind {
                OnboardKind::Create => "Name and protect your new wallet",
                OnboardKind::ImportPhrase => "Import a recovery phrase",
                OnboardKind::ImportKey => "Import a private key",
                OnboardKind::Watch => "Watch addresses",
            };
            let rect = centered(body, 84, fields.len() as u16 * 3 + 9);
            let inner = modal_frame(f, rect, t, heading);
            let mut lines = Vec::new();
            if *kind == OnboardKind::Create && !verified {
                lines.push(Line::from(Span::styled(
                    "! Phrase not verified. You can verify later: quai-terminal wallet verify-phrase",
                    Style::default().fg(t.attention),
                )));
                lines.push(Line::from(""));
            }
            let _ = push_fields(&mut lines, t, fields, *focus, None, None, inner.width);
            if let Some(b) = &app.busy {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(format!("{} {b}", spinner()), Style::default().fg(t.pending))));
            }
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
    }
}

/// Theme showroom: grouped list with swatches, live mock-up preview, contrast badge.
fn draw_showroom(f: &mut Frame, area: Rect, t: &Theme, picker: &Picker) {
    let [list_area, preview] = Layout::horizontal([Constraint::Length(40), Constraint::Min(30)]).areas(area);
    let visible = picker.visible();
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled("/ ", t.dim_style()),
        Span::styled(
            if picker.filter.is_empty() { "type to filter".to_string() } else { format!("{}▏", picker.filter) },
            if picker.filter.is_empty() { t.dim_style() } else { t.strong_style() },
        ),
    ])];
    let height = list_area.height.saturating_sub(2) as usize;
    let sel_pos = visible.iter().position(|&i| i == picker.selected).unwrap_or(0);
    let start = sel_pos.saturating_sub(height.saturating_sub(4));
    let mut family = String::new();
    for &i in visible.iter().skip(start) {
        let e = &picker.entries[i];
        if e.family != family {
            family = e.family.clone();
            lines.push(Line::from(Span::styled(format!("  {}", family.to_lowercase()), t.dim_style().add_modifier(Modifier::ITALIC))));
        }
        let active = i == picker.selected;
        let sw = |c: Color| Span::styled("■", Style::default().fg(c).bg(e.theme.surface));
        let mut spans = vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!(" {:<22}", truncate(&e.name, 22)), if active { t.strong_style().fg(t.focus) } else { t.text_style() }),
            Span::styled(" ", Style::default().bg(e.theme.surface)),
        ];
        for c in [e.theme.text, e.theme.focus, e.theme.quai, e.theme.qi, e.theme.ok, e.theme.danger] {
            spans.push(sw(c));
        }
        spans.push(Span::styled(" ", Style::default().bg(e.theme.surface)));
        lines.push(Line::from(spans));
        if lines.len() > height {
            break;
        }
    }
    if visible.is_empty() {
        lines.push(Line::from(Span::styled("  no theme matches", t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines), list_area);

    // Live preview in the candidate theme (already applied to `t`).
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(t.border(false))
        .style(t.base())
        .padding(Padding::new(2, 2, 1, 0));
    let inner = block.inner(preview);
    f.render_widget(block, preview);
    let name = picker.current().map(|e| e.name.clone()).unwrap_or_default();
    let contrast = t.text_contrast();
    let badge = match contrast {
        Some(c) if c >= 7.0 => Span::styled(format!("✓ contrast {c:.1}:1 AAA"), Style::default().fg(t.ok)),
        Some(c) => Span::styled(format!("✓ contrast {c:.1}:1 AA"), Style::default().fg(t.ok)),
        None => Span::styled("follows your terminal colors", t.dim_style()),
    };
    let digits = big_digits("1,204");
    let mut lines = vec![
        Line::from(vec![Span::styled(name, t.strong_style().fg(t.focus)), Span::raw("   "), badge]),
        Line::from(if t.adjusted {
            Span::styled("! some colors were adjusted for readability", Style::default().fg(t.attention))
        } else {
            Span::raw("")
        }),
        Line::from(vec![Span::styled("▌", Style::default().fg(t.quai)), Span::styled(" QUAI", t.dim_style())]),
    ];
    for (i, row) in digits.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled("▌ ", Style::default().fg(t.quai)),
            Span::styled(row.clone(), t.strong_style().fg(t.quai)),
            Span::styled(if i == 2 { " .5183" } else { "" }, t.dim_style()),
        ]));
    }
    lines.push(Line::from(vec![
        Span::styled("▌", Style::default().fg(t.qi)),
        Span::styled(" Qi ", t.dim_style()),
        Span::styled("386.286", t.strong_style().fg(t.qi)),
        Span::styled("   ◕ 3.416 locked", Style::default().fg(t.pending)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("✓ confirmed  ", Style::default().fg(t.ok)),
        Span::styled("○ pending  ", Style::default().fg(t.pending)),
        Span::styled("↩ refunded  ", Style::default().fg(t.attention)),
        Span::styled("× failed", Style::default().fg(t.danger)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" 2m ", t.dim_style()),
        Span::styled("↘ ", Style::default().fg(t.qi)),
        Span::raw("received 12.5 Qi → 0x00F4…804B"),
    ]));
    lines.push(Line::from(Span::styled(" 5m ↔ QUAI→Qi conversion of 100 QUAI", t.selected())));
    lines.push(Line::from(vec![
        Span::styled(" 9m ", t.dim_style()),
        Span::styled("↗ ", Style::default().fg(t.quai)),
        Span::raw("send of 1.25 QUAI to "),
        Span::styled("bob", Style::default().fg(t.link)),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("  Reject  ", t.selected()),
        Span::raw("   "),
        Span::styled("  Approve & sign  ", Style::default().fg(t.surface).bg(t.ok).add_modifier(Modifier::BOLD)),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
    let spark_area = Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width.min(40), 2);
    if spark_area.y > inner.y + 16 {
        f.render_widget(
            Sparkline::default()
                .data([3u64, 4, 3, 5, 6, 5, 7, 6, 8, 7, 9, 8, 7, 9, 10, 9, 11, 10, 12, 11])
                .style(Style::default().fg(t.focus)),
            spark_area,
        );
    }
}

/// Looks up "available X" for an amount field's asset.
type Available<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Effect list with a live, looping preview of the selection on the lock screen wordmark.
fn draw_gallery(f: &mut Frame, area: Rect, t: &Theme, g: &mut app::Gallery) {
    let [list, preview] = Layout::horizontal([Constraint::Length(24), Constraint::Min(30)]).areas(area);
    let names: Vec<&str> = std::iter::once("random").chain(super::fx::EFFECTS.iter().map(|(n, _)| *n)).collect();
    let height = list.height as usize;
    let start = g.selected.saturating_sub(height.saturating_sub(3));
    let lines: Vec<Line> = names
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(i, n)| {
            let active = i == g.selected;
            Line::from(vec![
                Span::styled(if active { "▌ " } else { "  " }, Style::default().fg(t.focus)),
                Span::styled(
                    if i == 0 { "random (default)".to_string() } else { (*n).to_string() },
                    if active { t.strong_style().fg(t.focus) } else { t.text_style() },
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), list);

    let [info, stage] = Layout::vertical([Constraint::Length(3), Constraint::Min(6)]).areas(preview);
    let description = if g.selected == 0 {
        "a different effect each time the screen locks".to_string()
    } else {
        super::fx::EFFECTS[g.selected - 1].1.to_string()
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(g.value(), t.strong_style().fg(t.focus))),
            Line::from(Span::styled(description, t.dim_style())),
        ]),
        info,
    );
    let stage_block = Block::default().borders(Borders::ALL).border_type(BorderType::Plain).border_style(t.border(false)).style(t.base());
    let canvas = stage_block.inner(stage);
    f.render_widget(stage_block, stage);
    // Loop the way the lock screen does: the preview restarts the instant it ends, so what the
    // gallery shows is what you get — no still wordmark sitting between runs.
    let wanted = if g.selected == 0 { None } else { Some(super::fx::EFFECTS[g.selected - 1].0) };
    let stale = g.preview.as_ref().is_some_and(|c| c.size() != (canvas.width, canvas.height));
    if stale {
        g.preview = None;
    }
    if g.preview.as_mut().is_none_or(|c| !c.advance()) {
        let name = wanted.map(str::to_string).unwrap_or_else(|| super::fx::random_lock_effect().to_string());
        let args = super::fx::theme_args(&name, t);
        g.preview = super::fx::Ceremony::with_args(&name, &args, super::fx::WORDMARK, canvas.width, canvas.height, 900);
        // First step now: a ceremony carries no frame until it is stepped.
        if let Some(c) = g.preview.as_mut() {
            c.step();
        }
    }
    if let Some(c) = &g.preview {
        let accent = t.accent_rgb.map(|(r, g, b)| Color::Rgb(r, g, b)).unwrap_or(t.focus);
        c.render(canvas, f.buffer_mut(), t.base().fg(accent));
    } else {
        wordmark(f, canvas, t);
    }
}

/// Form fields with an underline track, inline error, available-balance hint and strength meter.
/// Draw the fields, and report the line range the focused one occupies.
///
/// Only this knows it: a field is two lines, or three when it carries an error or a hint, so
/// anything computing it from the outside would drift the moment that changed. A form long enough
/// to scroll needs the range to keep the cursor on screen.
fn push_fields(
    lines: &mut Vec<Line<'static>>,
    t: &Theme,
    fields: &[app::Field],
    focus: usize,
    error: Option<(usize, &str)>,
    available: Option<&Available>,
    width: u16,
) -> std::ops::Range<usize> {
    let mut focused = 0..0;
    let label_w = fields.iter().map(|f| f.label.chars().count()).max().unwrap_or(10).max(10) + 2;
    let track_w = (width as usize).saturating_sub(label_w + 2).min(64);
    for (i, field) in fields.iter().enumerate() {
        let active = i == focus;
        let started = lines.len();
        let value = match &field.kind {
            k if field.is_secret() && *k != FieldKind::Text => "•".repeat(field.value.chars().count()),
            FieldKind::Choice(_) => format!("‹ {} ›", field.choice_label().unwrap_or(&field.value)),
            _ => field.value.clone(),
        };
        let shown = if value.chars().count() > track_w {
            format!("…{}", value.chars().rev().take(track_w.saturating_sub(2)).collect::<Vec<_>>().into_iter().rev().collect::<String>())
        } else {
            value
        };
        let label_style = if active { t.strong_style().fg(t.focus) } else { t.dim_style() };
        let mut spans = vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            Span::styled(format!("{:<label_w$}", field.label), label_style),
            Span::styled(
                format!("{shown}{}", if active && !matches!(field.kind, FieldKind::Choice(_)) { "▏" } else { "" }),
                if active { t.strong_style() } else { t.text_style() },
            ),
        ];
        // The hint is a placeholder on the input line, so it can't be mistaken for the next field.
        if field.value.is_empty() && !matches!(field.kind, FieldKind::Choice(_)) {
            let placeholder = match (field.hint.is_empty(), field.optional) {
                (true, true) => "optional".to_string(),
                (true, false) => String::new(),
                (false, true) => format!("{} · optional", field.hint),
                (false, false) => field.hint.clone(),
            };
            spans.push(Span::styled(truncate(&placeholder, track_w.saturating_sub(1)), t.dim_style().add_modifier(Modifier::ITALIC)));
        }
        lines.push(Line::from(spans));
        let track_style = if error.is_some_and(|(e, _)| e == i) {
            Style::default().fg(t.danger)
        } else if active {
            Style::default().fg(t.focus)
        } else {
            t.dim_style()
        };
        let mut under = vec![Span::raw(" ".repeat(label_w + 1)), Span::styled("▔".repeat(track_w), track_style)];
        if let FieldKind::NewSecret = field.kind
            && !field.value.is_empty()
        {
            let s = app::password_strength(&field.value);
            let (label, color) =
                [("too short", t.danger), ("weak", t.danger), ("fair", t.attention), ("good", t.ok), ("strong", t.ok)][s as usize];
            under = vec![
                Span::raw(" ".repeat(label_w + 1)),
                Span::styled("█".repeat(s as usize + 1), Style::default().fg(color)),
                Span::styled("░".repeat(4 - s as usize), t.dim_style()),
                Span::styled(format!(" {label}"), Style::default().fg(color)),
            ];
        }
        lines.push(Line::from(under));
        match error {
            Some((e, msg)) if e == i => lines.push(Line::from(vec![
                Span::raw(" ".repeat(label_w + 1)),
                Span::styled(format!("× {msg}"), Style::default().fg(t.danger)),
            ])),
            _ => {
                let mut info = String::new();
                if let (FieldKind::Amount(asset), Some(avail)) = (&field.kind, available)
                    && let Some(a) = avail(asset)
                {
                    info = format!("available {a}");
                } else if !field.value.is_empty() && matches!(field.kind, FieldKind::Text) && active {
                    info = field.hint.clone();
                }
                if active && !info.is_empty() {
                    lines.push(Line::from(vec![Span::raw(" ".repeat(label_w + 1)), Span::styled(info, t.dim_style())]));
                } else {
                    lines.push(Line::from(""));
                }
            }
        }
        if active {
            focused = started..lines.len();
        }
    }
    focused
}

// ---------------------------------------------------------------- modals

/// Token icons or the NFT thumbnail for a review, with their names as text beside them.
fn draw_review_pictures(f: &mut Frame, app: &App, t: &Theme, area: Rect, visuals: &[wallet_core::tx::ReviewVisual]) {
    // Only the review's own pictures are placed while it is open.
    app.eco.kitty.borrow_mut().clear();
    let bg = Style::default().bg(t.raised);
    if let Some(v) = visuals.iter().find(|v| v.role == "nft") {
        let id = v.token_id.clone().unwrap_or_default();
        let pic = Rect { width: 16.min(area.width), ..area };
        super::images::picture(app, f.buffer_mut(), pic, t, app.nft_image_url(&v.contract, &id).as_deref(), &v.symbol, &v.contract, true);
        let text = Rect { x: area.x + pic.width + 2, width: area.width.saturating_sub(pic.width + 2), ..area };
        let lines = vec![
            Line::from(Span::styled(v.symbol.clone(), t.strong_style())),
            Line::from(Span::styled(format!("token #{id}"), t.dim_style())),
            Line::from(Span::styled(short_address(&v.contract), Style::default().fg(t.link))),
        ];
        f.render_widget(Paragraph::new(lines).style(bg), text);
        return;
    }
    let mut x = area.x;
    for (i, v) in visuals.iter().enumerate() {
        if i > 0 {
            f.render_widget(Paragraph::new(Line::from(Span::styled(" → ", t.dim_style()))).style(bg), Rect::new(x, area.y + 1, 3, 1));
            x += 4;
        }
        let pic = Rect::new(x, area.y, 4, 2);
        if pic.right() > area.right() {
            break;
        }
        super::images::picture(app, f.buffer_mut(), pic, t, app.asset_icon_url(&v.contract).as_deref(), &v.symbol, &v.contract, false);
        let label = match v.role.as_str() {
            "pay" => "you pay",
            "receive" => "you receive",
            _ => "token",
        };
        let w = (v.symbol.chars().count().max(label.len()) as u16 + 1).min(area.right().saturating_sub(x + 5));
        let lines = vec![Line::from(Span::styled(label, t.dim_style())), Line::from(Span::styled(v.symbol.clone(), t.strong_style()))];
        f.render_widget(Paragraph::new(lines).style(bg), Rect::new(x + 5, area.y, w, 2));
        x += 5 + w + 1;
    }
}

fn draw_modal(f: &mut Frame, app: &mut App, t: &Theme, area: Rect) {
    let palette = match &app.modal {
        Modal::Palette { query, .. } => app.palette_entries(query),
        _ => Vec::new(),
    };
    let dash = &app.dash;
    match &mut app.modal {
        Modal::None => {}
        Modal::Orders { rows, selected } => super::order_ui::draw(f, area, t, rows, *selected),
        Modal::Form(form) => {
            let notes = u16::from(form.note.is_some()) + u16::from(form.contract_note.is_some());
            let h = form.fields.len() as u16 * 3 + 7 + notes * 2;
            let rect = centered(area, 84, h);
            let title = form.title.clone();
            let inner = modal_frame(f, rect, t, &title);
            let mut lines = Vec::new();
            // What the destination turned out to be goes first: it can change what this form is
            // even for, so it is read before the amount is typed.
            if let Some(n) = &form.contract_note {
                lines.push(Line::from(Span::styled(format!("◆ {n}"), Style::default().fg(t.attention))));
                lines.push(Line::from(""));
            }
            if let Some(n) = &form.note {
                lines.push(Line::from(Span::styled(n.clone(), Style::default().fg(t.attention))));
                lines.push(Line::from(""));
            }
            let account_value = form
                .fields
                .iter()
                .find(|fl| matches!(fl.kind, FieldKind::Choice(_)) && fl.value.starts_with("0x"))
                .map(|fl| fl.value.clone());
            let available = |asset: &str| -> Option<String> {
                match asset {
                    "QUAI" => {
                        let a = account_value
                            .as_ref()
                            .and_then(|v| dash.accounts.iter().find(|a| a.address == *v))
                            .or(dash.accounts.first())?;
                        Some(format!("{} QUAI", q(a.balance)))
                    }
                    "QI" => dash.qi.as_ref().map(|s| format!("{} Qi", qi(s.balance.spendable))),
                    "WQI" => dash.wrap.as_ref().and_then(|w| w.wqi_qi.clone()).map(|v| format!("{v} WQI")),
                    "WQUAI" => dash
                        .wrap
                        .as_ref()
                        .and_then(|w| w.wquai_atoms.clone())
                        .map(|v| format!("{} WQUAI", q(v.parse().unwrap_or_default()))),
                    _ => None,
                }
            };
            let error = form.error.as_deref().map(|e| (form.error_field.unwrap_or(usize::MAX), e));
            let focused = push_fields(&mut lines, t, &form.fields, form.focus, error, Some(&available), inner.width);
            if let (Some(msg), None) = (&form.error, form.error_field) {
                lines.push(Line::from(Span::styled(format!("× {msg}"), Style::default().fg(t.danger))));
            }
            lines.push(Line::from(if form.pending {
                Span::styled(
                    format!("{} preparing… nothing is signed until you approve the review", spinner()),
                    Style::default().fg(t.pending),
                )
            } else {
                Span::styled("tab/↑↓ fields · ←/→ choices · enter continue · esc cancel", t.dim_style())
            }));
            // A contract call can declare any number of arguments, so this is the first form whose
            // height is not known in advance. Rather than draw the tail of it off the bottom of
            // the screen — where the cursor would still move into fields nobody can see — the
            // content scrolls to keep the focused field in view.
            //
            // The offset is derived from the focus every frame rather than stored: there is no
            // second piece of state to fall out of step with the cursor.
            //
            // The footer keeps a row of its own below the scrolling part, because a form cut off
            // at the bottom must never read as a complete one — the line that says so has to stay
            // on screen, which it would not if it scrolled with everything else.
            let footer = lines.pop().unwrap_or_else(|| Line::from(""));
            let body = Rect { height: inner.height.saturating_sub(1), ..inner };
            let viewport = body.height as usize;
            let (scroll, above, below) = if lines.len() > viewport {
                let last = lines.len().saturating_sub(viewport);
                // Far enough down to show the whole focused field, and no further than the end.
                let scroll = focused.end.saturating_sub(viewport).min(last).min(focused.start);
                (scroll, scroll, lines.len().saturating_sub(scroll + viewport))
            } else {
                (0, 0, 0)
            };
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((scroll as u16, 0)).style(Style::default().bg(t.raised)),
                body,
            );
            let mut marks = Vec::new();
            if above > 0 {
                marks.push(format!("↑ {above} more"));
            }
            if below > 0 {
                marks.push(format!("↓ {below} more"));
            }
            let footer = if marks.is_empty() {
                footer
            } else {
                Line::from(vec![
                    Span::styled(format!("{}  ", marks.join(" · ")), Style::default().fg(t.attention)),
                    Span::styled("tab/↑↓ fields · enter continue · esc cancel", t.dim_style()),
                ])
            };
            let footer_row = Rect { y: inner.y + body.height, height: 1, ..inner };
            f.render_widget(Paragraph::new(footer).style(Style::default().bg(t.raised)), footer_row);
        }
        Modal::Review(r) => {
            let rv = &r.review;
            let mut lines: Vec<Line> = Vec::new();
            for w in &rv.warnings {
                // A first send is a moment to check, not an alarm; everything else is.
                let line = if w.starts_with("first time sending") {
                    Span::styled(format!("◌ {w}"), Style::default().fg(t.attention))
                } else if w.starts_with("possible address poisoning") || w.contains("only ever sent you dust") {
                    Span::styled(format!("⚠ {w}"), Style::default().fg(t.danger).add_modifier(Modifier::BOLD | Modifier::REVERSED))
                } else {
                    Span::styled(format!("! {w}"), Style::default().fg(t.danger).add_modifier(Modifier::BOLD))
                };
                lines.push(Line::from(line));
            }
            if !rv.warnings.is_empty() {
                lines.push(Line::from(""));
            }
            // The outcome first, in one glance: what leaves, what arrives, what the fee can be.
            if !rv.changes.is_empty() {
                lines.push(Line::from(Span::styled("balance changes", t.strong_style())));
                let amount_w = rv.changes.iter().map(|c| c.amount.chars().count()).max().unwrap_or(0);
                let asset_w = rv.changes.iter().map(|c| c.asset.chars().count()).max().unwrap_or(0).min(24);
                for c in &rv.changes {
                    let (sign, color) = match c.direction.as_str() {
                        "out" => ("−", t.danger),
                        "in" => ("+", t.ok),
                        "fee" => ("−", t.attention),
                        _ => ("·", t.dim),
                    };
                    let mut spans = vec![Span::styled(format!("  {sign} "), Style::default().fg(color).add_modifier(Modifier::BOLD))];
                    if c.direction != "none" {
                        spans.push(Span::styled(format!("{:>amount_w$} ", c.amount), t.strong_style()));
                        spans.push(Span::styled(format!("{:<asset_w$}   ", truncate(&c.asset, asset_w)), Style::default().fg(color)));
                    }
                    spans.push(Span::styled(c.note.clone(), t.dim_style()));
                    lines.push(Line::from(spans));
                }
                lines.push(Line::from(""));
            }
            let label_w = rv.fields.iter().map(|fl| fl.label.chars().count() + 2).max().unwrap_or(0).clamp(16, 44);
            let kv = |k: &str, v: String, style: Style| {
                Line::from(vec![Span::styled(format!("{k:<label_w$}"), t.dim_style()), Span::styled(v, style)])
            };
            let asset_color = if rv.asset.eq_ignore_ascii_case("QI") { t.qi } else { t.quai };
            lines.push(kv("network", rv.network.clone(), Style::default().fg(t.link)));
            lines.push(kv("from", rv.from.clone(), t.text_style()));
            lines.push(kv("to", rv.to.clone(), t.strong_style()));
            let amount_text = if rv.amount.contains(' ') { rv.amount.clone() } else { format!("{} {}", rv.amount, rv.asset) };
            lines.push(kv("amount", amount_text, t.strong_style().fg(asset_color)));
            // A fee above the fee policy is highlighted, never blocked: approving sends it.
            let fee_text =
                format!("{}{}", rv.max_fee, rv.fee_bps.map(|b| format!("  ({}.{:02}% of amount)", b / 100, b % 100)).unwrap_or_default());
            if rv.fee_over_policy {
                lines.push(Line::from(vec![
                    Span::styled(format!("{:<label_w$}", "max fee"), t.dim_style()),
                    Span::styled(fee_text, t.strong_style().fg(t.danger)),
                    Span::styled("  ▲ above fee policy ", Style::default().fg(t.surface).bg(t.danger).add_modifier(Modifier::BOLD)),
                ]));
            } else {
                lines.push(kv("max fee", fee_text, Style::default().fg(t.attention)));
            }
            for field in &rv.fields {
                let mut label = field.label.clone();
                if let Some(first) = label.get(..1) {
                    label = format!("{}{}", first.to_lowercase(), &label[1..]);
                }
                lines.push(kv(&label, field.value.clone(), t.text_style()));
            }
            // Plain-language outcome, and the worst-case balance afterwards when it's knowable.
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("what happens", t.strong_style())));
            for step in review_story(&rv.kind) {
                lines.push(Line::from(vec![Span::styled("  → ", Style::default().fg(t.focus)), Span::styled(step, t.text_style())]));
            }
            if rv.asset == "QUAI"
                && let Some(acct) = dash.accounts.iter().find(|a| rv.from.starts_with(&a.address))
                && let (Ok(amount_base), Some(fee_text)) = (rv.amount_base.parse::<U256>(), rv.max_fee.split_whitespace().next())
                && let Ok(fee) = amount::parse_quai(fee_text)
            {
                let spent = amount_base.saturating_add(fee);
                let after = acct.balance.saturating_sub(spent);
                lines.push(Line::from(vec![
                    Span::styled("  → ", Style::default().fg(t.focus)),
                    Span::styled(format!("{} keeps at least {} QUAI", acct.label, q(after)), t.text_style()),
                ]));
            }
            if !rv.coins.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("Qi inputs and outputs", t.strong_style())));
                for c in &rv.coins {
                    let (glyph, color) = match c.role.as_str() {
                        "input" => ("−", t.dim),
                        "change" => ("↩", t.ok),
                        _ => ("→", t.qi),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {glyph} {:<10}", c.role), Style::default().fg(color)),
                        Span::raw(format!("{:>16} Qi  ", amount::qi(U256::from(c.qits)))),
                        Span::styled(c.address.clone(), t.dim_style()),
                    ]));
                }
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("— end of review —", t.dim_style())).alignment(Alignment::Center));
            // Fit the modal to the content (wrapped at the modal's inner width) up to the screen.
            let est_width = 100u16.min(area.width.saturating_sub(2)).saturating_sub(6).max(1) as usize;
            let est_lines: usize = lines.iter().map(|l| l.width().max(1).div_ceil(est_width)).sum();
            // Pictures of the assets involved sit above the text (tokens: a 2-row strip; NFT: 8 rows).
            let visuals = rv.visuals.clone();
            let strip_h: u16 = if visuals.iter().any(|v| v.role == "nft") {
                8
            } else if visuals.is_empty() {
                0
            } else {
                3
            };
            let rect = centered(area, 100, (est_lines as u16 + 6 + strip_h).min(area.height.saturating_sub(4)));
            let title = format!("review · {}", rv.title);
            let inner = modal_frame(f, rect, t, &title);
            let [mut body, buttons] = Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);
            let mut strip = None;
            if strip_h > 0 && body.height >= strip_h + 8 && !app.plain {
                let [s, rest] = Layout::vertical([Constraint::Length(strip_h), Constraint::Min(3)]).areas(body);
                strip = Some(s);
                body = rest;
            }
            let width = body.width.max(1) as usize;
            r.content_lines = lines.iter().map(|l| l.width().max(1).div_ceil(width)).sum::<usize>() as u16;
            r.viewport = body.height;
            r.scroll = r.scroll.min(r.content_lines.saturating_sub(r.viewport));
            f.render_widget(
                Paragraph::new(lines).wrap(Wrap { trim: false }).scroll((r.scroll, 0)).style(Style::default().bg(t.raised)),
                body,
            );
            let can = r.can_approve();
            let reject = if r.approve_focused { Style::default().fg(t.text) } else { t.selected().add_modifier(Modifier::BOLD) };
            let approve = if !can {
                t.dim_style().add_modifier(Modifier::CROSSED_OUT)
            } else if r.approve_focused {
                Style::default().fg(t.surface).bg(t.ok).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.ok)
            };
            let meter_w = 14usize;
            let filled = (r.read_ratio() * meter_w as f64).round() as usize;
            let mut line = Line::from(vec![
                Span::styled(if r.approve_focused { "  Reject · tab  " } else { "  Reject · enter  " }, reject),
                Span::raw("   "),
                Span::styled(if r.approve_focused && can { "  Approve & sign · enter  " } else { "  Approve & sign · tab  " }, approve),
                Span::raw("   "),
                Span::styled("read ", t.dim_style()),
                Span::styled("█".repeat(filled), Style::default().fg(if can { t.ok } else { t.focus })),
                Span::styled("░".repeat(meter_w - filled), t.dim_style()),
                Span::styled(if can { "  esc rejects" } else { "  space/j to read on · esc rejects" }, t.dim_style()),
            ]);
            // Narrow: the hint shortens rather than losing its end.
            if line.width() > buttons.width as usize
                && let Some(last) = line.spans.last_mut()
            {
                *last = Span::styled(if can { "  esc" } else { "  space reads" }, t.dim_style());
                if line.width() > buttons.width as usize {
                    line.spans.pop();
                }
            }
            f.render_widget(Paragraph::new(vec![Line::from(""), line]).style(Style::default().bg(t.raised)), buttons);
            if let Some(strip) = strip {
                draw_review_pictures(f, app, t, strip, &visuals);
            }
        }
        Modal::Help => {
            let screen_hints = app::screen_hints(app.screen);
            let global: [(&str, &str); 15] = [
                ("1–5 0", "sections: Home Trade NFTs People Activity System"),
                ("[ ] · tab", "sub-tabs · move focus between panes"),
                ("enter · esc", "open detail · back"),
                ("t · o", "trade the focused token · copy explorer/Bazarr link"),
                ("j/k · g/G · ctrl-d/u", "move"),
                ("' + label", "jump to a labeled row"),
                (": · ctrl-p", "command palette with CLI equivalents"),
                ("tab · `", "into the pinned chat's message box (pin one on the Board with P)"),
                ("s / r", "send / receive"),
                ("c / C", "convert QUAI→Qi / Qi→QUAI"),
                ("R · ctrl-r", "refresh everything"),
                ("N", "notifications"),
                ("l", "lock now"),
                ("q", "quit"),
                ("review", "read to the end · tab to Approve · esc rejects · y copy as a command"),
            ];
            let moved = app.help_moved;
            let term_rows = super::glossary::for_screen(app.screen).len().min(6);
            let term_rows = if term_rows > 0 { term_rows as u16 + 3 } else { 0 };
            let rect = centered(area, 100, (global.len() + screen_hints.len()) as u16 + 9 + term_rows + if moved { 5 } else { 0 });
            let inner = modal_frame(f, rect, t, "keys");
            let mut lines = Vec::new();
            if moved {
                lines.push(Line::from(Span::styled("What moved", t.strong_style().fg(t.attention))));
                lines.push(Line::from(
                    "  1 Home: portfolio, Qi coins, accounts, locks (Assets is gone) · 2 Trade: swap, pools, convert, wrap",
                ));
                lines.push(Line::from(
                    "  3 NFTs · 4 People: contacts, channels, board · 5 Activity · 0 System: wallets, network, settings, data sources",
                ));
                lines.push(Line::from(Span::styled("  Tab now moves focus inside a screen; [ and ] change sub-tabs.", t.dim_style())));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(format!("on {}", app.breadcrumb().join(" › ")), t.strong_style())));
            for (k, v) in screen_hints {
                lines.push(Line::from(vec![Span::styled(format!("  {k:<24}"), t.strong_style().fg(t.focus)), Span::raw(*v)]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("everywhere", t.strong_style())));
            for (k, v) in global {
                lines.push(Line::from(vec![Span::styled(format!("  {k:<24}"), t.strong_style().fg(t.focus)), Span::raw(v)]));
            }
            let terms = super::glossary::for_screen(app.screen);
            if !terms.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("words here", t.strong_style())));
                for term in terms.iter().take(6) {
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<24}", term.word), t.strong_style().fg(t.qi)),
                        Span::raw(truncate(term.meaning, (inner.width as usize).saturating_sub(27))),
                    ]));
                }
                lines.push(Line::from(Span::styled("  g all words", t.dim_style())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Prefer plain text? Every action has a CLI command with --output json (see the palette).",
                t.dim_style(),
            )));
            lines.push(Line::from(Span::styled("any key closes", t.dim_style())));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Palette { query, selected } => {
            let rect = centered(area, 110, 26);
            let inner = modal_frame(f, rect, t, "command palette");
            let entries = palette;
            let mut lines = vec![
                Line::from(vec![Span::styled("› ", Style::default().fg(t.focus)), Span::styled(format!("{query}▏"), t.strong_style())]),
                Line::from(Span::styled(
                    if query.is_empty() { "try: send alice 5 quai · swap 10 wqi to usdt · markets · theme" } else { "" },
                    t.dim_style(),
                )),
            ];
            // Rows below the query, less one for the selected entry's CLI line.
            let rows = inner.height.saturating_sub(4) as usize;
            let start = selected.saturating_sub(rows.saturating_sub(1));
            let label_w = (inner.width as usize).saturating_sub(30).clamp(20, 48);
            let hint_w = (inner.width as usize).saturating_sub(label_w + 10).max(8);
            for (i, e) in entries.iter().enumerate().skip(start).take(rows) {
                let active = i == *selected;
                let style = if active { t.selected() } else { Style::default().bg(t.raised) };
                let tag_color = match e.tag {
                    "do" => t.ok,
                    "recent" => t.pending,
                    "contact" => t.link,
                    "asset" | "market" => t.quai,
                    "go" => t.focus,
                    "term" => t.qi,
                    "chat" => t.link,
                    _ => t.dim,
                };
                lines.push(Line::from(vec![
                    Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
                    Span::styled(format!("{:<7}", e.tag), Style::default().fg(tag_color)),
                    Span::styled(format!("{:<label_w$}", truncate(&e.label, label_w)), style),
                    Span::styled(format!(" {}", truncate(&e.hint, hint_w)), t.dim_style()),
                ]));
            }
            if entries.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!("Nothing matches among {} actions, screens, contacts and markets.", ACTIONS.len()),
                    t.dim_style(),
                )));
            }
            let [list, cli] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), list);
            // The same thing from a shell, or what a term means, for the selected entry.
            let footer = match entries.get(*selected) {
                Some(e) if e.tag == "term" => Line::from(Span::styled(format!("  {}", e.hint), t.text_style())),
                Some(e) if !e.cli.is_empty() => {
                    Line::from(vec![Span::styled("  $ ", t.dim_style()), Span::styled(e.cli.clone(), t.text_style())])
                }
                _ => Line::from(Span::styled("  enter run · ↑↓ choose · esc close", t.dim_style())),
            };
            f.render_widget(Paragraph::new(footer).style(Style::default().bg(t.raised)), cli);
        }
        Modal::Receive { asset_qi, account } => {
            let (asset_qi, account) = (*asset_qi, *account);
            draw_receive(f, app, t, area, asset_qi, account);
        }
        Modal::Secret { text, title } => {
            let rect = centered(area, 88, 16);
            let inner = modal_frame(f, rect, t, "recovery phrase");
            let words: Vec<&str> = text.split_whitespace().collect();
            let mut lines = vec![Line::from(Span::styled(title.clone(), Style::default().fg(t.danger))), Line::from("")];
            let rows = words.len().div_ceil(4);
            for r in 0..rows {
                let mut spans = Vec::new();
                for c in 0..4 {
                    if let Some(w) = words.get(c * rows + r) {
                        spans.push(Span::styled(format!("{:>4} ", c * rows + r + 1), t.dim_style()));
                        spans.push(Span::styled(format!("{w:<14}"), t.strong_style()));
                    }
                }
                lines.push(Line::from(spans));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled("any key hides the phrase and wipes it from memory", t.dim_style())));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Quote(qt) => {
            // The saturated and held cases trade the scenario bars for wrapped prose, so the height
            // has to follow the content rather than the scenario count.
            let extra = qt.hold.as_ref().map_or(0, |h| textwrap(&h.note, 84).len() + 1) + usize::from(qt.discount_saturated) * 5;
            let rect = centered(area, 88, (24 + qt.notes.len() + extra) as u16);
            let inner = modal_frame(f, rect, t, "conversion quote");
            let label_w = qt.scenarios.iter().map(|s| s.label.chars().count()).max().unwrap_or(20) + 2;
            let mut lines = vec![
                Line::from(Span::styled(qt.headline.clone(), t.strong_style().fg(t.focus))),
                Line::from(""),
                Line::from(vec![Span::styled("you send     ", t.dim_style()), Span::styled(qt.amount_display.clone(), t.strong_style())]),
                Line::from(vec![
                    Span::styled("you receive  ", t.dim_style()),
                    Span::styled(
                        qt.expected_display
                            .clone()
                            .map(|e| format!("about {e} expected"))
                            .or_else(|| qt.quoted_display.clone())
                            .unwrap_or_else(|| "unavailable".into()),
                        t.strong_style().fg(t.ok),
                    ),
                    Span::styled(
                        qt.quoted_display
                            .clone()
                            .filter(|_| qt.expected_display.is_some() && qt.implied_slippage_bps.is_some_and(|b| b > 0))
                            .map(|s| format!("   rate {s}"))
                            .unwrap_or_default(),
                        t.dim_style(),
                    ),
                ]),
                Line::from(vec![
                    Span::styled("block flow   ", t.dim_style()),
                    Span::raw(
                        qt.flow_amount
                            .clone()
                            .map(|v| format!("{} QUAI", q(v.parse().unwrap_or_default())))
                            .unwrap_or_else(|| "not reported".into()),
                    ),
                ]),
            ];
            // What the discount costs right now, which is the number the send/receive pair implies
            // but never states. Colour tracks severity so the size of the loss is legible at a
            // glance rather than read off two amounts.
            if let Some(bps) = qt.implied_slippage_bps.filter(|b| *b > 0) {
                let color = if bps >= 5000 {
                    t.danger
                } else if bps >= 500 {
                    t.attention
                } else {
                    t.ok
                };
                lines.push(Line::from(vec![
                    Span::styled("discount now ", t.dim_style()),
                    Span::styled(format!("{} below the rate", wallet_core::ops::percent(bps)), Style::default().fg(color)),
                ]));
            }
            lines.push(Line::from(""));
            if let Some(h) = &qt.hold {
                for l in textwrap(&h.note, 84) {
                    lines.push(Line::from(Span::styled(l, Style::default().fg(t.danger))));
                }
                lines.push(Line::from(""));
            }
            if qt.discount_saturated {
                // Four bars all reading 90% say nothing. One sentence that names the way out does.
                for l in textwrap(
                    "The discount is at its floor: at this size the protocol pays one tenth of the rate, and no slippage setting changes that. It grows with size against the block's conversion flow, so converting a smaller amount at a time loses far less — and the market route (wrap, swap, unwrap) is usually several times better.",
                    84,
                ) {
                    lines.push(Line::from(Span::styled(l, Style::default().fg(t.danger))));
                }
                lines.push(Line::from(""));
            } else if !qt.scenarios.is_empty() {
                lines.push(Line::from(Span::styled("if others convert in the same block…", t.strong_style())));
                let max_bps = qt.scenarios.iter().map(|s| s.discount_bps).max().unwrap_or(1).max(qt.suggested_slippage_bps).max(1);
                for s in &qt.scenarios {
                    let over = s.discount_bps > qt.suggested_slippage_bps;
                    let color = if over {
                        t.danger
                    } else if s.discount_bps > 100 {
                        t.attention
                    } else {
                        t.ok
                    };
                    let bar = (usize::from(s.discount_bps) * 30).div_ceil(usize::from(max_bps)).max(1);
                    lines.push(Line::from(vec![
                        Span::styled(format!("  {:<label_w$}", s.label), t.text_style()),
                        Span::styled(format!("{:>7} ", wallet_core::ops::percent(s.discount_bps)), Style::default().fg(color)),
                        Span::styled("▇".repeat(bar), Style::default().fg(color)),
                        Span::styled(if over { " refund risk" } else { "" }, Style::default().fg(t.danger)),
                    ]));
                }
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                Span::styled("suggested slippage ", t.dim_style()),
                Span::styled(
                    format!("{} ({} bps)", wallet_core::ops::percent(qt.suggested_slippage_bps), qt.suggested_slippage_bps),
                    t.strong_style().fg(t.focus),
                ),
            ]));
            if let Some(m) = &qt.minimum {
                lines.push(Line::from(vec![Span::styled("minimum            ", t.dim_style()), Span::raw(m.clone())]));
            }
            lines.push(Line::from(""));
            for n in &qt.notes {
                lines.push(Line::from(Span::styled(format!("· {n}"), t.dim_style())));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("c ", t.strong_style().fg(t.focus)),
                Span::styled("QUAI→Qi  ", t.dim_style()),
                Span::styled("C ", t.strong_style().fg(t.focus)),
                Span::styled("Qi→QUAI  ", t.dim_style()),
                Span::styled("esc ", t.strong_style().fg(t.focus)),
                Span::styled("close", t.dim_style()),
            ]));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Result(s) => {
            let rect = centered(area, 96, 12);
            let inner = modal_frame(f, rect, t, "submitted");
            let style = status_style(t, s.status);
            let mut lines = vec![
                Line::from(vec![
                    Span::styled(format!("{} ", status_glyph(s.status)), style),
                    Span::styled(s.message.clone(), t.strong_style()),
                ]),
                Line::from(""),
                Line::from(vec![Span::styled("tx         ", t.dim_style()), Span::raw(s.tx_hash.clone())]),
                Line::from(vec![Span::styled("operation  ", t.dim_style()), Span::raw(s.op_id.clone())]),
            ];
            if let Some(e) = &s.explorer {
                lines.push(Line::from(vec![
                    Span::styled("explorer   ", t.dim_style()),
                    Span::styled(e.clone(), Style::default().fg(t.link).add_modifier(Modifier::UNDERLINED)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Tracked on the activity screen (3). You'll get a notification when it lands. enter/esc close",
                t.dim_style(),
            )));
            f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Glossary { selected } => {
            let terms = super::glossary::TERMS;
            let rect = centered(area, 100, 30);
            let inner = modal_frame(f, rect, t, "glossary");
            let [list, meaning] = Layout::horizontal([Constraint::Length(24), Constraint::Min(20)]).areas(inner);
            let rows = list.height as usize;
            let start = selected.saturating_sub(rows.saturating_sub(1));
            let items: Vec<Line> = terms
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(i, term)| {
                    let style = if i == *selected { t.selected() } else { Style::default().bg(t.raised) };
                    Line::from(Span::styled(format!(" {:<22}", term.word), style))
                })
                .collect();
            f.render_widget(Paragraph::new(items).style(Style::default().bg(t.raised)), list);
            let term = &terms[(*selected).min(terms.len() - 1)];
            let mut body = vec![
                Line::from(Span::styled(term.word, t.strong_style().fg(t.qi))),
                Line::from(""),
                Line::from(Span::styled(term.meaning, t.text_style())),
                Line::from(""),
            ];
            if !term.screens.is_empty() {
                let wher: Vec<String> = term.screens.iter().map(|s| format!("{} › {}", s.section().title(), s.title())).collect();
                body.push(Line::from(vec![Span::styled("seen on  ", t.dim_style()), Span::raw(wher.join(" · "))]));
            }
            body.push(Line::from(""));
            body.push(Line::from(Span::styled("j/k move · any other key closes · : finds a word too", t.dim_style())));
            f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }).style(Style::default().bg(t.raised)), meaning);
        }
        Modal::Notifications => {
            let rect = centered(area, 96, 24);
            let inner = modal_frame(f, rect, t, "notifications");
            let lines: Vec<Line> = if dash.notifications.is_empty() {
                vec![Line::from(Span::styled("Quiet chain, quiet mind.", t.dim_style()))]
            } else {
                dash.notifications
                    .iter()
                    .take(inner.height as usize)
                    .map(|n| {
                        let text = format!("{} {}", n.title, n.body).to_lowercase();
                        let (g, c) = match n.level.as_str() {
                            "error" => ("×", t.danger),
                            "warn" => ("!", t.attention),
                            _ if text.contains("failed") || text.contains("refund") => ("×", t.danger),
                            _ if text.contains("settling") || text.contains("locked") || text.contains("submitted") => ("◕", t.pending),
                            "success" => ("✓", t.ok),
                            _ => ("●", t.focus),
                        };
                        let body = truncate(&n.body, (inner.width as usize).saturating_sub(n.title.chars().count() + 14));
                        Line::from(vec![
                            Span::styled(format!("{g} "), Style::default().fg(c)),
                            Span::styled(format!("{:>7}  ", ago(n.at)), t.dim_style()),
                            Span::styled(n.title.clone(), if n.read { t.text_style() } else { t.strong_style() }),
                            Span::styled(format!("  {body}"), t.dim_style()),
                        ])
                    })
                    .collect()
            };
            let mut lines = lines;
            if !app.log.is_empty() {
                lines.truncate((inner.height as usize).saturating_sub(app.log.len().min(6) + 2));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled("recent messages", t.strong_style())));
                for m in app.log.iter().take(6) {
                    let (g, c) = if m.error { ("×", t.danger) } else { ("✓", t.ok) };
                    lines.push(Line::from(vec![
                        Span::styled(format!("{g} "), Style::default().fg(c)),
                        Span::styled(truncate(&m.text, inner.width as usize - 2), t.text_style()),
                    ]));
                }
            }
            lines.truncate((inner.height as usize).saturating_sub(2));
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Confirmations, receipts and alerts collect here · opening marks them read · 5 Activity has the full history · esc close",
                t.dim_style(),
            )));
            f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), inner);
        }
        Modal::Confirm { title, body, .. } => {
            let rect = centered(area, 64, 8);
            let title = title.clone();
            let inner = modal_frame(f, rect, t, &title);
            f.render_widget(
                Paragraph::new(vec![
                    Line::from(body.clone()),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("  y  ", t.selected()),
                        Span::styled(" yes    ", t.dim_style()),
                        Span::styled("  n  ", Style::default().fg(t.text)),
                        Span::styled(" no (default)", t.dim_style()),
                    ]),
                ])
                .wrap(Wrap { trim: true })
                .style(Style::default().bg(t.raised)),
                inner,
            );
        }
        Modal::Themes(picker) => {
            let rect = centered(area, 116, 32);
            let inner = modal_frame(f, rect, t, "theme showroom · ↑↓ preview · type to filter · enter use · esc revert");
            draw_showroom(f, inner, t, picker);
        }
        Modal::TokenPicker { pay, query, selected } => {
            let (pay, query, selected) = (*pay, query.clone(), *selected);
            let rect = centered(area, 84, 22);
            let inner = modal_frame(f, rect, t, if pay { "you pay · pick a token" } else { "you receive · pick a token" });
            super::views::draw_token_picker(f, app, t, inner, &query, selected, pay);
        }
        Modal::Effects(gallery) => {
            let rect = centered(area, 116, 32);
            let inner = modal_frame(f, rect, t, "lock screen gallery · ↑↓ preview · enter use · esc close");
            draw_gallery(f, inner, t, gallery);
        }
    }
}

fn group4(s: &str) -> String {
    let body = s.strip_prefix("0x").unwrap_or(s);
    let groups: Vec<String> = body.chars().collect::<Vec<_>>().chunks(4).map(|c| c.iter().collect()).collect();
    if s.starts_with("0x") { format!("0x {}", groups.join(" ")) } else { groups.join(" ") }
}

fn draw_receive(f: &mut Frame, app: &mut App, t: &Theme, area: Rect, asset_qi: bool, account: usize) {
    let (data, subtitle) = if asset_qi {
        match app.meta.as_ref().and_then(|m| m.payment_code.clone()) {
            Some(code) => (code, "Payment code · every payment gets a fresh address · n new plain Qi address"),
            None => match app.dash.qi_addresses.last() {
                Some((_, a, _)) => (a.clone(), "Plain Qi address (reuse reduces privacy) · n new address"),
                None => (String::new(), "no Qi receive address yet · n new address"),
            },
        }
    } else {
        match app.dash.accounts.get(account) {
            Some(a) => (a.address.clone(), "Cyprus-1 · send only QUAI and Quai tokens here · j/k other account"),
            None => (String::new(), "no accounts yet"),
        }
    };
    let rect = centered(area, 76, area.height.saturating_sub(2).min(42));
    let inner = modal_frame(f, rect, t, "receive");
    let tab =
        |on: bool, color: Color| if on { Style::default().fg(t.surface).bg(color).add_modifier(Modifier::BOLD) } else { t.dim_style() };
    let switch = Line::from(vec![
        Span::styled(" ", tab(!asset_qi, t.quai)),
        super::images::native_span(app, t, "quai"),
        Span::styled(" QUAI ", tab(!asset_qi, t.quai)),
        Span::raw(" "),
        Span::styled(" ", tab(asset_qi, t.qi)),
        super::images::native_span(app, t, "qi"),
        Span::styled(" Qi ", tab(asset_qi, t.qi)),
        Span::styled("   tab switch", t.dim_style()),
    ]);
    let label = app.dash.accounts.get(account).filter(|_| !asset_qi).map(|a| format!("{} · ", a.label)).unwrap_or_default();
    let text_lines = if data.starts_with("0x") { 1 } else { (data.len() as u16).div_ceil(inner.width.max(1)) };
    let [top, qr_area, text_area] =
        Layout::vertical([Constraint::Length(2), Constraint::Min(5), Constraint::Length(text_lines + 3)]).areas(inner);
    f.render_widget(Paragraph::new(switch).alignment(Alignment::Center).style(Style::default().bg(t.raised)), top);
    let shown = if data.starts_with("0x") { group4(&data) } else { data.clone() };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(shown, t.strong_style().fg(if asset_qi { t.qi } else { t.quai }))),
            Line::from(""),
            Line::from(Span::styled(format!("{label}{subtitle}"), t.dim_style())),
            Line::from(Span::styled("y copy · esc close", t.dim_style())),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(t.raised)),
        text_area,
    );
    if data.is_empty() {
        return;
    }
    match app.caps.tier {
        Tier::Pixels => {
            let rows = qr_area.height.min(qr_area.width / 2).max(8);
            let cols = ((rows as u32 * app.caps.cell_px.1 as u32) / app.caps.cell_px.0.max(1) as u32) as u16;
            let cols = cols.min(qr_area.width);
            let r = Rect::new(qr_area.x + (qr_area.width - cols) / 2, qr_area.y, cols, rows.min(qr_area.height));
            app.qr_rect = Some((r, data));
        }
        Tier::Cells => {
            let fit = [4usize, 2].into_iter().find_map(|quiet| {
                let (size, grid) = super::terminal::qr_modules(&data, quiet)?;
                ((size as u16) <= qr_area.width && (size.div_ceil(2) as u16) <= qr_area.height).then_some((size, grid))
            });
            match fit {
                Some((size, grid)) => paint_qr(f.buffer_mut(), qr_area, size, &grid),
                None => empty_state(f, qr_area, t, "▪", "Enlarge the terminal to show the QR code.", &[]),
            }
        }
        Tier::Text => empty_state(f, qr_area, t, "▪", "QR codes are off in text mode.", &[]),
    }
}

/// Half-block QR, always dark-on-white for scanners.
fn paint_qr(buf: &mut Buffer, area: Rect, size: usize, grid: &[bool]) {
    let h = size.div_ceil(2) as u16;
    let x0 = area.x + (area.width - size as u16) / 2;
    let y0 = area.y + (area.height - h) / 2;
    for row in 0..h as usize {
        for col in 0..size {
            let top = grid[row * 2 * size + col];
            let bottom = row * 2 + 1 < size && grid[(row * 2 + 1) * size + col];
            let ch = match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            };
            if let Some(cell) = buf.cell_mut((x0 + col as u16, y0 + row as u16)) {
                cell.set_char(ch).set_fg(Color::Black).set_bg(Color::White);
            }
        }
    }
}

/// Whether the current frame needs periodic redraws.
pub fn wants_animation(app: &App) -> bool {
    app.busy.is_some()
        // The pending pill's spinner turns while a transaction waits to be mined.
        || (app.motion().effects() && !app.confirming_ops().is_empty())
        || app.beat.is_some_and(|b| b.elapsed().as_millis() < 1000)
        || (app.motion() != Motion::Off && (app.animating() || app.eco.fading()))
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "ui_nft_grid_tests.rs"]
mod nft_grid;
