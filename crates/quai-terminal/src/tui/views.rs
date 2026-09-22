//! Views for the ecosystem sections: Home, the exchange cards, NFTs, data
//! sources and the detail stack. Shared widgets come from `ui`.

use super::app::{self, App, Detail, Screen, Section};
use super::eco::WRAP_MODES;
use super::images;
use super::theme::Theme;
use super::ui::{
    activity_table_rows, ago, ago_short, big_digits, empty_state, hero_fits, incoming_text, panel, spinner, status_glyph, truncate,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Axis, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table, Wrap};
use wallet_core::amount;
use wallet_core::appdb::Activity;
use wallet_core::portfolio::{AssetKey, AssetRow, PriceKind, Trust};
use wallet_core::sdk::U256;
use wallet_core::session::short_address;
use wallet_core::swap::SwapAsset;
use wallet_core::track::describe;

const BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

fn key(t: &Theme, k: &str) -> Span<'static> {
    Span::styled(format!("{k} "), t.strong_style().fg(t.focus))
}

fn kv<'a>(t: &Theme, k: &str, v: impl Into<Span<'a>>) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<12}"), t.dim_style()), v.into()])
}

/// A `kv` line with an icon in front of the value.
fn kv_icon<'a>(t: &Theme, k: &str, icon: Span<'static>, v: impl Into<Span<'a>>) -> Line<'a> {
    Line::from(vec![Span::styled(format!("{k:<12}"), t.dim_style()), icon, Span::raw(" "), v.into()])
}

/// Icon for a listing's payment currency (native QUAI or a token the network knows).
fn currency_span(app: &App, t: &Theme, l: &wallet_core::market::Listing) -> Option<Span<'static>> {
    if l.is_native() {
        return Some(images::native_span(app, t, "quai"));
    }
    let network = app.config.network(&app.network_id).ok()?;
    let (symbol, _) = wallet_core::market::known_currency(&network, &l.currency)?;
    Some(images::asset_span(app, t, &l.currency, symbol))
}

fn trust_span(t: &Theme, trust: Trust) -> Span<'static> {
    match trust {
        Trust::Verified => Span::styled("✓", Style::default().fg(t.ok)),
        Trust::Unverified => Span::styled("⚠", Style::default().fg(t.attention)),
        Trust::Unknown => Span::styled("·", t.dim_style()),
    }
}

fn src_span(t: &Theme, r: &AssetRow, stale: bool) -> Span<'static> {
    let (glyph, style) = match r.price_kind {
        PriceKind::Market => ("●", Style::default().fg(t.ok)),
        PriceKind::Protocol => ("◈", Style::default().fg(t.qi)),
        PriceKind::None if r.trust == Trust::Unverified => ("⚠", Style::default().fg(t.attention)),
        PriceKind::None => ("—", t.dim_style()),
    };
    let age = wallet_core::registry::now().saturating_sub(r.price_at);
    if stale || (r.price_at > 0 && age > 900) {
        Span::styled(format!("◔ {}", wallet_core::track::human_duration(age)), t.dim_style())
    } else {
        Span::styled(glyph, style)
    }
}

fn balance_text(r: &AssetRow) -> String {
    let text = amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4));
    if r.exact { text } else { format!("{text} ~") }
}

/// QUAI keeps its blue and Qi its magenta; tokens take their icon's color (contrast-guarded).
fn asset_color(app: &App, t: &Theme, r: &AssetRow) -> Color {
    match &r.key {
        AssetKey::Quai => t.quai,
        AssetKey::Qi => t.qi,
        AssetKey::Token(a) => images::token_tint(app, t, r.icon_url.as_deref(), &r.symbol, a),
    }
}

fn contract_of(r: &AssetRow) -> String {
    match &r.key {
        AssetKey::Token(a) => a.clone(),
        k => k.id(),
    }
}

/// One-line activity description for token/NFT transfer rows and receipts.
pub fn activity_text(a: &Activity) -> String {
    if a.detail["sale"].as_bool() == Some(true) {
        let name = a.detail["name"].as_str().unwrap_or("NFT");
        let decimals = a.detail["decimals"].as_u64().unwrap_or(18) as u8;
        let v: U256 = a.amount.parse().unwrap_or_default();
        return format!("sold {name} for {} {}", amount::group_thousands(&amount::format_amount_short(v, decimals, 4)), a.asset);
    }
    if a.detail["source"].as_str() == Some("explorer") {
        let standard = a.detail["standard"].as_str().unwrap_or("ERC-20");
        let verb = if a.direction == "in" { "received" } else { "sent" };
        if standard != "ERC-20" {
            let name = a.detail["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(&a.asset);
            return format!("{verb} {name} #{}", a.detail["token_id"].as_str().unwrap_or("?"));
        }
        let decimals = a.detail["decimals"].as_u64().unwrap_or(18) as u8;
        let v: U256 = a.amount.parse().unwrap_or_default();
        return format!("{verb} {} {}", amount::group_thousands(&amount::format_amount_short(v, decimals, 4)), a.asset);
    }
    format!("{} {}", wallet_core::track::incoming_verb(a), incoming_text(a))
}

// ---------------------------------------------------------------- shell

/// Sub-tab strip at the top of the content area.
pub fn draw_tabs(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let section = app.screen.section();
    let active = if section == Section::Activity {
        app::ActivityFilter::ALL.iter().position(|x| *x == app.activity_filter).unwrap_or(0)
    } else {
        section.screens(&app.config.features).iter().position(|s| *s == app.screen).unwrap_or(0)
    };
    let mut spans = vec![Span::styled(format!(" {} ", section.title()), t.dim_style()), Span::styled("· ", t.dim_style())];
    for (i, label) in section.tab_labels(&app.config.features).iter().enumerate() {
        if i == active {
            spans.push(Span::styled(format!("▸ {label}"), t.strong_style().fg(t.focus)));
        } else {
            spans.push(Span::styled(label.to_string(), t.dim_style()));
        }
        spans.push(Span::raw("  "));
    }
    spans.push(Span::styled("[ ]", t.dim_style()));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Context actions for the top detail view, less those whose feature is off.
pub fn detail_hints(app: &App, d: &Detail) -> Vec<(&'static str, &'static str)> {
    let mut hints = all_detail_hints(app, d);
    if !app.config.features.trading {
        hints.retain(|(_, what)| !matches!(*what, "swap" | "trade" | "buy" | "sell"));
    }
    hints
}

fn all_detail_hints(app: &App, d: &Detail) -> Vec<(&'static str, &'static str)> {
    match d {
        Detail::Asset(id) if id == "quai" => {
            vec![("s", "send"), ("r", "receive"), ("b", "buy"), ("S", "sell"), ("t", "trade"), ("c", "convert"), ("w", "wrap")]
        }
        Detail::Asset(id) if id == "qi" => vec![("s", "send"), ("r", "receive"), ("c", "convert"), ("w", "wrap")],
        Detail::Asset(_) => {
            vec![("s", "send"), ("b", "buy"), ("S", "sell"), ("t", "trade"), ("w", "wrap/unwrap"), ("y", "copy")]
        }
        Detail::Nft(c, id) => {
            let mut h = Vec::new();
            if matches!(&app.eco.nfts, Some(Ok(v)) if v.iter().any(|n| n.item.contract == *c && n.item.token_id == *id)) {
                if app.my_listing(c, id).is_some() {
                    h.extend([("L", "change price"), ("X", "cancel listing")]);
                } else {
                    h.push(("L", "list for sale"));
                }
                h.push(("T", "transfer"));
            }
            if app.listing_for(c, id).is_some_and(|l| l.buyable()) {
                h.push(("b", "buy"));
            }
            h.extend([("o", "Bazarr link"), ("y", "copy contract")]);
            h
        }
        Detail::Collection(_) if app.eco.collection_listings_focused => {
            vec![("j/k", "listing"), ("enter", "open"), ("b", "buy"), ("tab", "items"), ("o", "explorer link")]
        }
        Detail::Collection(_) => vec![("hjkl", "move"), ("enter", "item"), ("tab", "listings"), ("o", "explorer link")],
        Detail::Activity(_) => vec![("o", "explorer link"), ("y", "copy")],
    }
}

pub fn draw_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, d: &Detail) {
    match d {
        Detail::Asset(id) => draw_asset_detail(f, app, t, area, id),
        Detail::Nft(c, id) => draw_nft_detail(f, app, t, area, c, id),
        Detail::Collection(c) => draw_collection_detail(f, app, t, area, c),
        Detail::Activity(k) => draw_activity_detail(f, app, t, area, k),
    }
}

// ---------------------------------------------------------------- Home

fn sparkline(values: &[f64], width: usize) -> String {
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let (min, max) = values.iter().fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    let span = (max - min).max(1e-12);
    let step = values.len() as f64 / width.min(values.len()) as f64;
    (0..width.min(values.len()))
        .map(|i| {
            let v = values[((i as f64) * step) as usize];
            if max - min < 1e-9 { BARS[3] } else { BARS[(((v - min) / span) * 7.0).round().clamp(0.0, 7.0) as usize] }
        })
        .collect()
}

pub fn draw_home(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // The hero is a glance; the holdings table and the attention/activity column carry the detail.
    let hero_h = 7.min(area.height.saturating_sub(6)).max(3);
    let [top, bottom] = Layout::vertical([Constraint::Length(hero_h), Constraint::Min(4)]).areas(area);
    let block = panel(t, "portfolio", app.pane == 0);
    let inner = block.inner(top);
    f.render_widget(block, top);
    match (&app.eco.portfolio, &app.eco.portfolio_error) {
        (Some(p), _) => {
            let total = amount::usd(p.total_usd);
            let change = p.change_7d.map(|c| format!("{}{c:.1}%", if c >= 0.0 { "+" } else { "" })).unwrap_or_default();
            let spark = sparkline(&p.history.iter().map(|v| v.usd).collect::<Vec<_>>(), 16);
            let whole = total.trim_start_matches('$').split('.').next().unwrap_or("0").to_string();
            let big = app.config.big_numbers && !app.plain && hero_fits(inner.width.saturating_sub(34), inner.height, &[&whole]);
            let mut y = inner.y;
            if big {
                let rows = big_digits(&whole);
                let frac = total.split_once('.').map(|(_, f)| format!(".{f}")).unwrap_or_default();
                for (i, row) in rows.iter().enumerate() {
                    let mut spans = vec![
                        Span::styled(if i == 0 { "▌$ " } else { "▌  " }, Style::default().fg(t.focus)),
                        Span::styled(row.clone(), t.strong_style().fg(t.focus)),
                    ];
                    if i == 2 {
                        spans.push(Span::styled(frac.clone(), t.dim_style()));
                    }
                    f.render_widget(Paragraph::new(Line::from(spans)), Rect { y, height: 1, ..inner });
                    y += 1;
                }
            } else {
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("▌ ", Style::default().fg(t.focus)),
                        Span::styled(total.clone(), t.strong_style().fg(t.focus)),
                    ])),
                    Rect { y, height: 1, ..inner },
                );
                y += 1;
            }
            let mut right = super::edge::spark_spans(app, t, &spark, t.focus);
            right.push(Span::raw(" "));
            let right = Line::from(
                right
                    .into_iter()
                    .chain([
                        Span::styled(if p.history.is_empty() { String::new() } else { "7d  ".into() }, t.dim_style()),
                        Span::styled(change, Style::default().fg(if p.change_7d.unwrap_or(0.0) >= 0.0 { t.ok } else { t.danger })),
                    ])
                    .collect::<Vec<_>>(),
            );
            f.render_widget(Paragraph::new(right).alignment(Alignment::Right), Rect { y: inner.y, height: 1, ..inner });
            if p.stale {
                f.render_widget(
                    Paragraph::new(Span::styled("◔ some prices from cache", t.dim_style())).alignment(Alignment::Right),
                    Rect { y: inner.y + 1, height: 1, ..inner },
                );
            }
            // NFT thumbnails (reference only, never in the total) on the right when there is room.
            let thumbs: Vec<(String, String, String, String)> = match &app.eco.nfts {
                Some(Ok(v)) if app.config.features.nfts && app.config.images && !app.plain => v
                    .iter()
                    .filter_map(|n| {
                        n.item.image.clone().map(|img| (n.item.contract.clone(), n.item.token_id.clone(), img, n.item.name.clone()))
                    })
                    .take(3)
                    .collect(),
                _ => Vec::new(),
            };
            let tile = (8u16, 4u16);
            let thumbs_w = if !thumbs.is_empty() && inner.width >= 120 && inner.bottom() > y + 1 + tile.1 {
                thumbs.len() as u16 * (tile.0 + 1) + 1
            } else {
                0
            };
            if thumbs_w > 0 {
                let x0 = inner.right() - thumbs_w + 1;
                for (i, (contract, _, img, name)) in thumbs.iter().enumerate() {
                    let rect = Rect::new(x0 + i as u16 * (tile.0 + 1), y + 1, tile.0, tile.1);
                    images::picture(app, f.buffer_mut(), rect, t, Some(img), name, contract, true);
                }
            }
            // The holdings themselves are the table below; the hero keeps only what a glance
            // wants: the NFT count, and the price provenance when `i` is open.
            let more_y = inner.bottom().saturating_sub(1);
            let mut more = Vec::new();
            if p.nfts.items > 0 {
                more.push(Span::styled("◧ ", Style::default().fg(t.qi)));
                more.push(Span::styled(
                    format!("{} NFTs in {} collections · not counted in the total  ", p.nfts.items, p.nfts.collections),
                    t.dim_style(),
                ));
                more.push(Span::styled("3", Style::default().fg(t.focus)));
                more.push(Span::styled(" to view", t.dim_style()));
            }
            if app.eco.info_open
                && let Some(b) = &p.prices
            {
                more = vec![Span::styled(
                    format!(
                        "QUAI {} · {} via {} · {}  ·  Qi {} · protocol-derived",
                        b.quai_usd.map(amount::usd_price).unwrap_or_else(|| "—".into()),
                        b.quai_source,
                        p.sources.join(", "),
                        ago(b.taken_at),
                        b.qi_usd.map(amount::usd_price).unwrap_or_else(|| "—".into())
                    ),
                    t.dim_style(),
                )];
            }
            f.render_widget(Paragraph::new(Line::from(more)), Rect { y: more_y, height: 1, ..inner });
        }
        (None, Some(e)) => {
            empty_state(f, inner, t, "×", &format!("Portfolio unavailable: {}", app::friendly_error(e)), &[("0 ]]", "data sources")])
        }
        (None, None) if app.dash.accounts.is_empty() => empty_state(f, inner, t, spinner(), "Loading balances…", &[]),
        (None, None) => empty_state(f, inner, t, spinner(), "Pricing your holdings…", &[]),
    }

    // Holdings on the left (pane 0), what needs doing and what just happened on the right (pane 1).
    let stacked = bottom.width < 100;
    let [holdings, right] = if stacked {
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(bottom)
    } else {
        // The side column is a fixed width and the table takes the slack, so the table keeps its
        // price and allocation columns instead of collapsing to the narrow layout.
        Layout::horizontal([Constraint::Min(60), Constraint::Length(46)]).areas(bottom)
    };
    draw_holdings(f, app, t, holdings, app.pane == 0);
    let mut items: Vec<Line> = Vec::new();
    let op_unlocks: Vec<u64> = app
        .dash
        .ops
        .iter()
        .filter(|o| o.status == wallet_core::appdb::OpStatus::Locked)
        .filter_map(|o| o.detail["unlock_height"].as_u64())
        .collect();
    for l in app.dash.locks.iter().filter(|l| !l.unlocked && !l.unlock_height.is_some_and(|h| op_unlocks.contains(&h))).take(3) {
        let eta = l.eta_secs.map(|s| format!(" in {}", wallet_core::track::human_duration(s))).unwrap_or_default();
        items.push(Line::from(vec![
            Span::styled("◕ ", Style::default().fg(t.pending)),
            Span::raw(format!("{} {} unlocks{eta}", l.amount, l.asset)),
        ]));
    }
    if let Some(qits) = app.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.as_deref()).and_then(|q| q.parse::<U256>().ok())
        && !qits.is_zero()
    {
        items.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(t.attention)),
            Span::raw(format!("{} Qi wrapped, ready to claim as WQI (3 ]])", amount::qi(qits))),
        ]));
    }
    if let Some(flow) = &app.eco.flow {
        let state = if flow.waiting.is_some() {
            "waiting for the approval to confirm"
        } else if flow.review_op.is_some() || flow.requested {
            "review open"
        } else {
            "preparing the next step"
        };
        items.push(Line::from(vec![
            Span::styled("↔ ", Style::default().fg(t.pending)),
            Span::raw(format!("{} · step {} · {state}", flow.kind.label(), flow.steps.max(1))),
        ]));
    }
    // Unfinished operations, by what they wait on; only unmined ones can use the user.
    use wallet_core::appdb::OpStatus;
    let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
    let open: Vec<&wallet_core::appdb::Operation> = app.dash.ops.iter().filter(|o| !o.status.is_terminal()).collect();
    let confirming = open.iter().filter(|o| !matches!(o.status, OpStatus::Settling | OpStatus::Locked)).count();
    if confirming > 0 {
        items.push(Line::from(vec![
            Span::styled("◌ ", Style::default().fg(t.pending)),
            Span::raw(format!("{confirming} transaction(s) waiting to be mined · ")),
            Span::styled("5 Activity", Style::default().fg(t.focus)),
        ]));
    }
    let settling = open.iter().filter(|o| o.status == OpStatus::Settling).count();
    if settling > 0 {
        items.push(Line::from(vec![
            Span::styled("◕ ", Style::default().fg(t.pending)),
            Span::raw(format!("{settling} settling on the destination chain · nothing to do")),
        ]));
    }
    for o in open.iter().filter(|o| o.status == OpStatus::Locked).take(2) {
        let eta = o.detail["unlock_height"]
            .as_u64()
            .filter(|u| *u > head && head > 0)
            .map(|u| format!(" in ~{}", wallet_core::track::human_duration((u - head) * 5)))
            .unwrap_or_default();
        items.push(Line::from(vec![
            Span::styled("◕ ", Style::default().fg(t.pending)),
            Span::raw(format!("{} unlocks{eta} · automatic, nothing to do", truncate(&describe(o), 30))),
        ]));
    }
    // Messages waiting in the channels this wallet follows, from wherever you are.
    let waiting: u32 = app.config.board_channels.iter().map(|c| app.board_unread(c)).sum();
    if waiting > 0 {
        items.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(t.focus)),
            Span::raw(format!("{waiting} new message(s) on the board · ")),
            Span::styled("5 ]] People › Board", Style::default().fg(t.focus)),
        ]));
    }
    let unread = app.dash.notifications.iter().filter(|n| !n.read).count();
    if unread > 0 {
        items.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(t.attention)),
            Span::raw(format!("{unread} unread notification(s) · ")),
            Span::styled("N to read", Style::default().fg(t.focus)),
        ]));
    }
    if app.meta.as_ref().is_some_and(|m| m.kind == wallet_core::registry::WalletKind::Hd && !m.backed_up) {
        items.push(Line::from(vec![Span::styled("! ", Style::default().fg(t.attention)), Span::raw("recovery phrase not verified")]));
    }
    if let Some(p) = &app.eco.portfolio {
        if p.stale {
            items.push(Line::from(vec![Span::styled("◔ ", t.dim_style()), Span::raw("some data is stale (source did not answer)")]));
        }
        for n in p.notices.iter().take(2) {
            items.push(Line::from(vec![Span::styled("× ", Style::default().fg(t.danger)), Span::raw(truncate(n, 60))]));
        }
        if p.unpriced > 0 {
            items.push(Line::from(vec![
                Span::styled("· ", t.dim_style()),
                Span::styled(format!("{} holding(s) without a price", p.unpriced), t.dim_style()),
            ]));
        }
    }
    if app.dash.node_error.is_some() {
        items.push(Line::from(vec![Span::styled("× ", Style::default().fg(t.danger)), Span::raw("node unreachable")]));
    }
    // Attention takes the rows it needs and no more. With nothing to say it is not drawn at all:
    // an empty box is noise, and "all clear" fits in the activity panel's title.
    let recent = if items.is_empty() {
        right
    } else {
        let want = (items.len() as u16 + 2).min(right.height.saturating_sub(5).max(3));
        let [attention, recent] = Layout::vertical([Constraint::Length(want), Constraint::Min(3)]).areas(right);
        f.render_widget(Paragraph::new(items.clone()).block(panel(t, &format!("attention · {}", items.len()), false)), attention);
        recent
    };
    let title = if items.is_empty() { "recent activity · ✓ all clear".to_string() } else { "recent activity".to_string() };
    let block = panel(t, &title, app.pane == 1);
    let inner = block.inner(recent);
    f.render_widget(block, recent);
    // when (4) · arrow (1) · description · status mark (1), with a cell between each.
    let text_w = inner.width.saturating_sub(4 + 1 + 1 + 3) as usize;
    let mut rows = activity_table_rows(app, t, inner.height as usize, false, Some(text_w));
    if rows.is_empty() {
        empty_state(
            f,
            inner,
            t,
            "○",
            "Quiet chain, quiet mind. Sends, receipts, swaps and NFT moves appear here as they happen.",
            &[("r", "receive"), ("t", "trade")],
        );
        return;
    }
    if app.pane == 1 {
        rows = rows.into_iter().enumerate().map(|(i, r)| if i == app.selected { r.style(t.selected()) } else { r }).collect();
    }
    f.render_widget(
        Table::new(rows, [Constraint::Length(4), Constraint::Length(1), Constraint::Min(10), Constraint::Length(1)]).column_spacing(1),
        inner,
    );
}

// ---------------------------------------------------------------- Home › holdings

/// The holdings table. Lives on Home, which is the only place it is drawn.
pub fn draw_holdings(f: &mut Frame, app: &App, t: &Theme, area: Rect, focused: bool) {
    let Some(p) = &app.eco.portfolio else {
        let block = panel(t, "holdings", focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        match &app.eco.portfolio_error {
            Some(e) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "refresh")]),
            None => empty_state(f, inner, t, spinner(), "Pricing your holdings…", &[]),
        }
        return;
    };
    // The legend explains the marks; it is there when asked for (`i`), not holding a third of the
    // screen for someone who read it once.
    let wide = area.width >= 110 && app.eco.info_open;
    let [list, side] =
        if wide { Layout::horizontal([Constraint::Min(70), Constraint::Length(32)]).areas(area) } else { [area, Rect::default()] };
    let block = panel(t, &format!("holdings · {} · {}", p.rows.len(), amount::usd(p.total_usd)), focused);
    let mut inner = block.inner(list);
    f.render_widget(block, list);
    // Wrapped Qi is not WQI until it is claimed; say so where people look for it.
    if let Some(qits) = app.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.as_deref()).and_then(|q| q.parse::<U256>().ok())
        && !qits.is_zero()
        && inner.height > 3
    {
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("◆ ", Style::default().fg(t.attention)),
                Span::raw(format!("{} Qi wrapped and waiting to be claimed as WQI · ", amount::qi(qits))),
                Span::styled("3 ]] Wrap › Claim WQI", Style::default().fg(t.focus)),
            ])),
            Rect { height: 1, ..inner },
        );
        inner = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    }
    let narrow = inner.width < 84;
    let header = if narrow {
        vec!["", "asset", "balance", "value", "src"]
    } else {
        vec!["", "asset", "balance", "price", "value", "alloc", "24h", "src"]
    };
    let icon_col: Vec<(Rect, &AssetRow)> = Vec::new();
    let mut icons = icon_col;
    let visible = inner.height.saturating_sub(1) as usize;
    let offset = app.selected.saturating_sub(visible.saturating_sub(1));
    let rows: Vec<Row> = p
        .rows
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, r)| {
            icons.push((Rect::new(inner.x, inner.y + 1 + (i - offset) as u16, 2, 1), r));
            let mut cells = vec![
                Cell::from("  "),
                Cell::from(Line::from(vec![
                    trust_span(t, r.trust),
                    Span::raw(" "),
                    Span::styled(truncate(&r.symbol, 10), t.strong_style().fg(asset_color(app, t, r))),
                ])),
                Cell::from(Line::from(balance_text(r)).alignment(Alignment::Right)),
            ];
            if !narrow {
                cells.push(Cell::from(
                    Line::from(r.price_usd.map(amount::usd_price).unwrap_or_else(|| "—".into())).alignment(Alignment::Right),
                ));
            }
            cells.push(Cell::from(Line::from(r.value_usd.map(amount::usd).unwrap_or_else(|| "—".into())).alignment(Alignment::Right)));
            if !narrow {
                let bar = if r.value_usd.is_some() {
                    format!("{:>3.0}% {}", r.allocation * 100.0, BARS[((r.allocation * 7.0).round() as usize).min(7)])
                } else {
                    "—".into()
                };
                cells.push(Cell::from(bar));
                cells.push(Cell::from(match r.change_24h {
                    Some(c) => Span::styled(
                        format!("{}{c:.1}%", if c >= 0.0 { "+" } else { "" }),
                        Style::default().fg(if c >= 0.0 { t.ok } else { t.danger }),
                    ),
                    None => Span::styled("—", t.dim_style()),
                }));
            }
            cells.push(Cell::from(src_span(t, r, p.stale)));
            let row = Row::new(cells);
            if i == app.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let widths: Vec<Constraint> = if narrow {
        vec![Constraint::Length(2), Constraint::Length(13), Constraint::Min(12), Constraint::Length(11), Constraint::Length(6)]
    } else {
        vec![
            Constraint::Length(2),
            Constraint::Length(13),
            Constraint::Min(14),
            Constraint::Length(11),
            Constraint::Length(11),
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Length(6),
        ]
    };
    f.render_widget(Table::new(rows, widths).column_spacing(1).header(Row::new(header).style(t.dim_style())), inner);
    // Where the value sits, as one bar in each asset's own colour — drawn in the room a short list
    // leaves, so a small portfolio fills its panel with something worth reading.
    let used = 1 + p.rows.len().min(visible) as u16;
    if inner.height >= used + 5 && inner.width >= 30 {
        let y = inner.y + used + 1;
        draw_allocation(f, app, t, Rect { y, height: inner.bottom() - y, ..inner }, p);
    }
    for (rect, r) in icons {
        images::picture(app, f.buffer_mut(), rect, t, app.row_icon(r).as_deref(), &r.symbol, &contract_of(r), false);
    }
    if side.width == 0 {
        return;
    }
    let mut lines = vec![Line::from(Span::styled("legend", t.strong_style()))];
    lines.push(Line::from(vec![Span::styled("✓ ", Style::default().fg(t.ok)), Span::raw("verified contract")]));
    lines.push(Line::from(vec![Span::styled("⚠ ", Style::default().fg(t.attention)), Span::raw("unverified — never auto-trusted")]));
    lines.push(Line::from(vec![Span::styled("● ", Style::default().fg(t.ok)), Span::raw("market price")]));
    lines.push(Line::from(vec![Span::styled("◈ ", Style::default().fg(t.qi)), Span::raw("protocol-derived (Qi)")]));
    lines.push(Line::from(vec![Span::styled("◔ ", t.dim_style()), Span::raw("stale, with age")]));
    lines.push(Line::from(vec![Span::styled("~ ", t.dim_style()), Span::raw("indexer balance (not re-read)")]));
    lines.push(Line::from(""));
    if let Some(b) = &p.prices {
        let price = |p: Option<f64>| Span::raw(p.map(amount::usd_price).unwrap_or_else(|| "—".into()));
        lines.push(Line::from(vec![
            images::native_span(app, t, "quai"),
            Span::styled(format!(" {:<9}", "QUAI"), t.dim_style()),
            price(b.quai_usd),
        ]));
        lines.push(Line::from(vec![
            images::native_span(app, t, "qi"),
            Span::styled(format!(" {:<9}", "Qi"), t.dim_style()),
            price(b.qi_usd),
        ]));
        lines.push(kv(t, "source", Span::styled(p.sources.join(", "), t.dim_style())));
        lines.push(kv(t, "updated", Span::styled(ago(p.observed_at), t.dim_style())));
    } else {
        lines.push(Line::from(Span::styled("prices off or unavailable", t.dim_style())));
    }
    if p.nfts.items > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(format!("◧ {} NFTs · not in total", p.nfts.items), t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "about prices", false)), side);
}

/// The portfolio's allocation: a stacked bar across the width, then the largest holdings named
/// with their share. Anything under half a percent is folded into "other".
fn draw_allocation(f: &mut Frame, app: &App, t: &Theme, area: Rect, p: &wallet_core::portfolio::Portfolio) {
    let mut parts: Vec<(&AssetRow, f64)> = p.rows.iter().filter(|r| r.value_usd.is_some()).map(|r| (r, r.allocation)).collect();
    parts.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total: f64 = parts.iter().map(|x| x.1).sum();
    if total <= 0.0 || area.height < 3 {
        return;
    }
    let width = area.width as f64;
    let (major, minor): (Vec<_>, Vec<_>) = parts.into_iter().partition(|x| x.1 / total >= 0.005);
    let other: f64 = minor.iter().map(|x| x.1).sum();
    f.render_widget(Paragraph::new(Span::styled("allocation", t.dim_style())), Rect { height: 1, ..area });
    // Cells per part, largest first, so rounding never pushes the bar past the width.
    // Each part its own colour: an asset's tint where it has one, and a distinct theme colour for
    // the ones that would otherwise share the dim grey of an unverified token.
    let spare = [t.link, t.pending, t.qi, t.ok, t.attention, t.strong, t.danger];
    let mut colours: Vec<Color> = Vec::new();
    for (r, _) in &major {
        let own = asset_color(app, t, r);
        // Greys (an unverified token's dimmed tint, or a greyscale icon) all read as the same
        // colour in a bar; so does a colour already used.
        let grey = match own {
            Color::Rgb(r, g, b) => r.max(g).max(b) - r.min(g).min(b) < 24,
            Color::Gray | Color::DarkGray | Color::White | Color::Black => true,
            _ => false,
        };
        let colour = if own == t.dim || grey || colours.contains(&own) {
            spare.iter().copied().find(|c| !colours.contains(c)).unwrap_or(own)
        } else {
            own
        };
        colours.push(colour);
    }
    let mut spans = Vec::new();
    let mut used = 0usize;
    for (i, (_, share)) in major.iter().enumerate() {
        let last = i + 1 == major.len() && other <= 0.0;
        let cells = if last { area.width as usize - used } else { ((share / total) * width).round().max(1.0) as usize };
        let cells = cells.min(area.width as usize - used);
        used += cells;
        spans.push(Span::styled("█".repeat(cells), Style::default().fg(colours[i])));
    }
    if other > 0.0 && used < area.width as usize {
        spans.push(Span::styled("░".repeat(area.width as usize - used), t.dim_style()));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect { y: area.y + 1, height: 1, ..area });
    let mut legend = Vec::new();
    for (i, (r, share)) in major.iter().enumerate().take(6) {
        legend.push(Span::styled("■ ", Style::default().fg(colours[i])));
        legend.push(Span::raw(format!("{} {:.0}%   ", truncate(&r.symbol, 10), share / total * 100.0)));
    }
    if other > 0.0 || major.len() > 6 {
        let rest: f64 = other + major.iter().skip(6).map(|x| x.1).sum::<f64>();
        legend.push(Span::styled(format!("░ other {:.1}%", rest / total * 100.0), t.dim_style()));
    }
    let legend_h = 1 + u16::from(legend.iter().map(|s| s.content.chars().count()).sum::<usize>() > area.width as usize);
    f.render_widget(Paragraph::new(Line::from(legend)).wrap(Wrap { trim: true }), Rect { y: area.y + 2, height: legend_h, ..area });
    // And what the value has done this week, where there is room for a chart rather than a glyph.
    let chart_y = area.y + 2 + legend_h + 1;
    if area.bottom() > chart_y + 6 && p.history.len() >= 2 {
        draw_value_chart(f, t, Rect { y: chart_y, height: area.bottom() - chart_y, ..area }, &p.history, p.change_7d);
    }
}

/// The portfolio's value over the history the portfolio carries (seven days), as a line with its
/// range on the axis. Honest about what it is: QUAI's balance history at today's price, other
/// holdings held constant — the hero's caption says so, and so does this one.
fn draw_value_chart(f: &mut Frame, t: &Theme, area: Rect, history: &[wallet_core::portfolio::ValuePoint], change: Option<f64>) {
    let points: Vec<(f64, f64)> = history.iter().map(|v| (v.at as f64, v.usd)).collect();
    let (x0, x1) = (points.first().map_or(0.0, |p| p.0), points.last().map_or(1.0, |p| p.0));
    let (lo, hi) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let pad = ((hi - lo) * 0.1).max(hi.abs() * 0.01).max(0.01);
    let (lo, hi) = ((lo - pad).max(0.0), hi + pad);
    let up = change.unwrap_or(0.0) >= 0.0;
    let colour = if up { t.ok } else { t.danger };
    let title = Line::from(vec![
        Span::styled("value · 7 days ", t.dim_style()),
        Span::styled(change.map(|c| format!("{}{c:.1}%", if up { "+" } else { "" })).unwrap_or_default(), Style::default().fg(colour)),
        Span::styled("  QUAI balance history at today's price", t.dim_style()),
    ]);
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });
    let chart_area = Rect { y: area.y + 1, height: area.height - 1, ..area };
    let dataset = Dataset::default()
        .graph_type(GraphType::Line)
        .marker(ratatui::symbols::Marker::Braille)
        .style(Style::default().fg(colour))
        .data(&points);
    let day = |at: f64| {
        let days = (wallet_core::registry::now() as f64 - at) / 86_400.0;
        if days < 0.5 { "now".to_string() } else { format!("{days:.0}d ago") }
    };
    let chart = Chart::new(vec![dataset])
        .x_axis(
            Axis::default()
                .bounds([x0, x1.max(x0 + 1.0)])
                .labels(vec![Span::styled(day(x0), t.dim_style()), Span::styled(day(x1), t.dim_style())])
                .style(t.dim_style()),
        )
        .y_axis(
            Axis::default()
                .bounds([lo, hi])
                .labels(vec![Span::styled(amount::usd(lo), t.dim_style()), Span::styled(amount::usd(hi), t.dim_style())])
                .style(t.dim_style()),
        );
    f.render_widget(chart, chart_area);
}

fn draw_asset_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, id: &str) {
    let row = app.eco.portfolio.as_ref().and_then(|p| p.rows.iter().find(|r| r.key.id() == id));
    let Some(r) = row else {
        let block = panel(t, id, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        empty_state(f, inner, t, spinner(), "Loading…", &[]);
        return;
    };
    let full = area.width < 80;
    let [left, right] = if full {
        [area, Rect::default()]
    } else {
        Layout::horizontal([Constraint::Min(40), Constraint::Length(44.min(area.width / 2))]).areas(area)
    };
    let block = panel(t, &format!("{} · {}", r.symbol, r.name), true);
    let inner = block.inner(left);
    f.render_widget(block, left);
    let pic = Rect::new(inner.x, inner.y, 8.min(inner.width), 4.min(inner.height));
    images::picture(app, f.buffer_mut(), pic, t, app.row_icon(r).as_deref(), &r.symbol, &contract_of(r), false);
    let text_area = Rect { x: inner.x + 10, width: inner.width.saturating_sub(10), height: 4.min(inner.height), ..inner };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(balance_text(r), t.strong_style().fg(asset_color(app, t, r))),
                Span::styled(format!(" {}", r.symbol), t.dim_style()),
            ]),
            Line::from(Span::styled(r.value_usd.map(amount::usd).unwrap_or_else(|| "unpriced".into()), t.strong_style())),
            Line::from(vec![
                trust_span(t, r.trust),
                Span::styled(
                    match r.trust {
                        Trust::Verified => " verified",
                        Trust::Unverified => " unverified contract",
                        Trust::Unknown => " not checked",
                    },
                    t.dim_style(),
                ),
            ]),
        ]),
        text_area,
    );
    let mut lines = Vec::new();
    lines.push(kv(t, "price", Span::raw(r.price_usd.map(amount::usd_price).unwrap_or_else(|| "—".into()))));
    if !r.price_source.is_empty() {
        lines.push(kv(t, "source", Span::styled(truncate(&r.price_source, 50), t.dim_style())));
        lines.push(kv(t, "observed", Span::styled(ago(r.price_at), t.dim_style())));
    }
    if let Some(c) = r.change_24h {
        lines.push(kv(
            t,
            "24h (cap)",
            Span::styled(
                format!("{}{c:.2}%", if c >= 0.0 { "+" } else { "" }),
                Style::default().fg(if c >= 0.0 { t.ok } else { t.danger }),
            ),
        ));
    }
    lines.push(kv(t, "allocation", Span::raw(format!("{:.1}%", r.allocation * 100.0))));
    if let AssetKey::Token(address) = &r.key {
        lines.push(kv(t, "contract", Span::styled(address.clone(), Style::default().fg(t.link))));
        match app.eco.token_info.get(address) {
            Some(Ok((info, verified))) => {
                lines.push(kv(t, "holders", Span::raw(info.holders.map(|h| h.to_string()).unwrap_or_else(|| "—".into()))));
                lines.push(kv(t, "decimals", Span::raw(info.decimals.map(|d| d.to_string()).unwrap_or_else(|| r.decimals.to_string()))));
                lines.push(kv(
                    t,
                    "source code",
                    Span::raw(match verified {
                        Some(true) => "verified on explorer",
                        Some(false) => "not verified",
                        None => "—",
                    }),
                ));
            }
            Some(Err(e)) => lines.push(kv(t, "explorer", Span::styled(truncate(e, 50), t.dim_style()))),
            None => lines.push(kv(t, "explorer", Span::styled(format!("{} loading…", spinner()), t.dim_style()))),
        }
    }
    if !r.exact {
        lines.push(Line::from(Span::styled("~ balance from the indexer; the node could not be read", Style::default().fg(t.attention))));
    }
    if id == "quai"
        && let Some(p) = &app.eco.portfolio
        && !p.history.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled("7d value ", t.dim_style())]));
        let spark = sparkline(&p.history.iter().map(|v| v.usd).collect::<Vec<_>>(), inner.width.saturating_sub(12) as usize);
        if let Some(last) = lines.last_mut() {
            last.spans.extend(super::edge::spark_spans(app, t, &spark, t.quai));
        }
    }
    let body = Rect { y: inner.y + 5, height: inner.height.saturating_sub(5), ..inner };
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), body);
    if right.width == 0 {
        return;
    }
    // Recent transfers of this asset.
    let block = panel(t, "recent transfers", false);
    let rinner = block.inner(right);
    f.render_widget(block, right);
    let token = match &r.key {
        AssetKey::Token(a) => Some(a.as_str()),
        _ => None,
    };
    let mut lines: Vec<Line> = Vec::new();
    for a in app.dash.activity.iter().filter(|a| match token {
        Some(addr) => a.detail["token"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(addr)),
        None => a.asset.eq_ignore_ascii_case(&r.symbol),
    }) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:<6} ", ago(a.observed)), t.dim_style()),
            Span::raw(truncate(&activity_text(a), 34)),
        ]));
    }
    for o in app
        .dash
        .ops
        .iter()
        .filter(|o| o.asset.eq_ignore_ascii_case(&r.symbol) || o.detail["token"].as_str().is_some_and(|x| Some(x) == token))
    {
        lines.push(Line::from(vec![Span::styled(format!("{:<6} ", ago(o.created)), t.dim_style()), Span::raw(truncate(&describe(o), 34))]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("no transfers seen yet", t.dim_style())));
    }
    lines.truncate(rinner.height as usize);
    f.render_widget(Paragraph::new(lines), rinner);
}

// ---------------------------------------------------------------- exchange cards

fn card_row<'a>(t: &Theme, focused: bool, label: &str, value: Vec<Span<'a>>) -> Line<'a> {
    let mut spans = vec![
        Span::styled(if focused { "▌ " } else { "  " }, Style::default().fg(t.focus)),
        Span::styled(format!("{label:<12}"), t.dim_style()),
    ];
    spans.extend(value);
    Line::from(spans)
}

fn amount_span(t: &Theme, text: &str, focused: bool) -> Span<'static> {
    let shown = if text.is_empty() { "0".to_string() } else { text.to_string() };
    if focused {
        Span::styled(format!("{shown}▏"), t.strong_style())
    } else {
        Span::styled(shown, if text.is_empty() { t.dim_style() } else { t.strong_style() })
    }
}

fn available(app: &App, asset: &SwapAsset) -> Option<String> {
    let p = app.eco.portfolio.as_ref()?;
    let id = match asset {
        SwapAsset::Quai => "quai".to_string(),
        SwapAsset::Token { address, .. } => address.clone(),
    };
    let r = p.rows.iter().find(|r| r.key.id() == id)?;
    Some(amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 6)))
}

fn asset_chip(app: &App, t: &Theme, asset: Option<&SwapAsset>) -> Vec<Span<'static>> {
    match asset {
        Some(a) => {
            let contract = match a {
                SwapAsset::Quai => "quai".to_string(),
                SwapAsset::Token { address, .. } => address.clone(),
            };
            let verified = match a {
                SwapAsset::Quai => true,
                SwapAsset::Token { address, .. } => {
                    app.eco
                        .portfolio
                        .as_ref()
                        .and_then(|p| p.rows.iter().find(|r| r.key.id() == *address))
                        .is_some_and(|r| r.trust == Trust::Verified)
                        || app
                            .config
                            .network(&app.network_id)
                            .ok()
                            .is_some_and(|n| wallet_core::portfolio::curated_addresses(&n).contains(address))
                }
            };
            vec![
                images::asset_span(app, t, &contract, a.symbol()),
                Span::raw(" "),
                Span::styled(format!("[{} {}▾]", a.symbol(), if verified { "✓" } else { "⚠" }), t.strong_style().fg(t.focus)),
            ]
        }
        None => vec![Span::styled("[pick ▾]", t.dim_style())],
    }
}

fn step_line(t: &Theme, steps: &[&str], current: usize) -> Line<'static> {
    let mut spans = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" → ", t.dim_style()));
        }
        let style = if i < current {
            Style::default().fg(t.ok)
        } else if i == current {
            t.strong_style().fg(t.focus)
        } else {
            t.dim_style()
        };
        spans.push(Span::styled(format!("{}{} {s}", if i < current { "✓" } else { "" }, i + 1), style));
    }
    Line::from(spans)
}

pub fn draw_swap(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let card = &app.eco.swap;
    let stacked = area.width < 100;
    let [left, right] = if stacked {
        Layout::vertical([Constraint::Length(12), Constraint::Min(6)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area)
    };
    let network = app.config.network(&app.network_id).ok();
    if network.as_ref().is_none_or(|n| n.ecosystem.quainance_router.is_none()) {
        let block = panel(t, "swap", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        empty_state(f, inner, t, "↔", "No swap router on this network (mainnet only).", &[("]", "convert"), ("]]", "wrap")]);
        return;
    }
    let quote = if app.swap_quote_current() { card.quote.as_ref() } else { None };
    let ok_quote = quote.and_then(|q| q.as_ref().ok());
    let receive = match ok_quote {
        Some(q) => Span::styled(
            format!(
                "≈ {}",
                amount::group_thousands(&amount::format_amount_short(q.amount_out.parse().unwrap_or_default(), q.to.decimals(), 6))
            ),
            t.strong_style(),
        ),
        None if !card.amount.is_empty() => Span::styled(format!("{} quoting…", spinner()), t.dim_style()),
        None => Span::styled("—", t.dim_style()),
    };
    let mut lines = vec![
        Line::from(Span::styled("you pay", t.dim_style())),
        card_row(t, card.field == 0, "token", asset_chip(app, t, Some(&card.from))),
        card_row(
            t,
            card.field == 1,
            "amount",
            vec![
                amount_span(t, &card.amount, card.field == 1),
                Span::styled(available(app, &card.from).map(|a| format!("   available {a}")).unwrap_or_default(), t.dim_style()),
            ],
        ),
        preset_line(t, card.preset),
        Line::from(Span::styled("you receive", t.dim_style())),
        card_row(t, card.field == 2, "token", asset_chip(app, t, card.to.as_ref())),
        card_row(t, false, "amount", vec![receive]),
        Line::from(""),
        card_row(
            t,
            card.field == 3,
            "slippage",
            vec![Span::styled(format!("{:.2}% ‹›", f64::from(card.slippage_bps) / 100.0), t.text_style())],
        ),
        card_row(t, card.field == 4, "deadline", vec![Span::styled(format!("{} min ‹›", card.deadline_minutes), t.text_style())]),
    ];
    if let Some(q) = ok_quote
        && (q.approval_needed || card.approving)
    {
        lines.push(Line::from(""));
        lines.push(step_line(t, &[&format!("approve exact {}", q.pay_text()), "swap"], 0));
        if card.approving {
            lines.push(Line::from(Span::styled(format!("{} waiting for the approval to confirm…", spinner()), t.dim_style())));
        }
    } else if ok_quote.is_some() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("enter · review the swap", t.strong_style().fg(t.focus))));
    }
    // Wide and tall: the form takes what it needs and the pair's chart fills the rest.
    let form_h = lines.len() as u16 + 2;
    let (form, chart) = if !stacked && left.height >= form_h + 12 {
        let [a, b] = Layout::vertical([Constraint::Length(form_h), Constraint::Min(10)]).areas(left);
        (a, Some(b))
    } else {
        (left, None)
    };
    // Beside Markets (the trader layout) the card is lit only while its screen has the keys.
    f.render_widget(Paragraph::new(lines).block(panel(t, "swap · Quainance", app.screen != Screen::Markets)), form);
    let pair = app.swap_pool();
    if let Some(area) = chart {
        draw_swap_chart(f, app, t, area, pair.as_ref());
    }

    let block = panel(t, "quote", false);
    let inner = block.inner(right);
    f.render_widget(block, right);
    let mut q_lines = Vec::new();
    match quote {
        Some(Ok(q)) => {
            q_lines.push(kv(t, "route", Span::raw(q.route_text())));
            if q.legs.len() > 1 {
                q_lines.push(kv(
                    t,
                    "",
                    Span::styled("two swaps, each reviewed · the second is sized from the first", Style::default().fg(t.attention)),
                ));
            }
            q_lines.push(kv(t, "you get", Span::styled(format!("≈ {}", q.receive_text()), t.strong_style())));
            q_lines.push(kv(
                t,
                if q.legs.len() > 1 { "estimate bound" } else { "minimum" },
                Span::raw(format!("{}  (slip {:.2}%)", q.minimum_text(), f64::from(q.slippage_bps) / 100.0)),
            ));
            if q.legs.len() > 1 {
                q_lines.push(kv(t, "", Span::styled("final output is not atomic; each leg has its own reviewed minimum", t.dim_style())));
            }
            let input = q.amount_in.parse::<f64>().ok().map(|v| v / 10f64.powi(q.from.decimals() as i32));
            let output = q.amount_out.parse::<f64>().ok().map(|v| v / 10f64.powi(q.to.decimals() as i32));
            if let Some(price) = input.zip(output).and_then(|(i, o)| (i > 0.0 && (o / i).is_finite()).then_some(o / i)) {
                q_lines.push(kv(
                    t,
                    "effective price",
                    Span::raw(format!("{} {} per {}", fmt_price(price), q.to.symbol(), q.from.symbol())),
                ));
            }
            let filled = ((q.impact_bps as f64 / 500.0) * 10.0).round().clamp(0.0, 10.0) as usize;
            let impact_color = if q.impact_bps >= wallet_core::swap::IMPACT_WARN_BPS { t.attention } else { t.ok };
            q_lines.push(kv(
                t,
                "impact",
                Span::styled(
                    format!("{}{}  {:.2}%", "■".repeat(filled), "□".repeat(10 - filled), q.impact_bps as f64 / 100.0),
                    Style::default().fg(impact_color),
                ),
            ));
            q_lines.push(kv(t, "fee", Span::raw(format!("{:.1}% LP · gas at review", q.fee_bps as f64 / 100.0))));
            q_lines.push(kv(t, "pools", Span::raw(q.pools.iter().map(|p| short_address(&p.pair)).collect::<Vec<_>>().join(" · "))));
            if let Some(l) = q.liquidity_text() {
                let thin = q.pools.iter().any(|p| p.tvl_usd.is_some_and(|v| v < wallet_core::swap::THIN_POOL_USD));
                q_lines.push(kv(t, "liquidity", Span::styled(l, if thin { Style::default().fg(t.attention) } else { t.text_style() })));
            }
            let venues: Vec<&str> = q.legs.iter().map(|l| l.venue.label()).collect();
            let venues = if venues.is_empty() { "Quainance".to_string() } else { venues.join(", then ") };
            q_lines.push(kv(t, "router", Span::styled(format!("✓ {venues} (pinned bytecode)"), Style::default().fg(t.ok))));
            q_lines.push(kv(
                t,
                "approval",
                if q.approval_needed {
                    Span::styled(format!("needed · exact {}", q.pay_text()), Style::default().fg(t.attention))
                } else {
                    Span::styled("not needed", t.dim_style())
                },
            ));
            let age = card.quoted_at.map(|a| a.elapsed().as_secs()).unwrap_or(0);
            q_lines.push(kv(
                t,
                "quoted",
                Span::styled(
                    if age < 5 { "just now".into() } else { format!("{age}s ago") },
                    if age > 30 { t.dim_style() } else { t.text_style() },
                ),
            ));
            for w in &q.warnings {
                q_lines.push(Line::from(Span::styled(format!("! {w}"), Style::default().fg(t.attention))));
            }
        }
        Some(Err(e)) => q_lines.push(Line::from(Span::styled(format!("× {}", app::friendly_error(e)), Style::default().fg(t.danger)))),
        None => {
            // Before any amount: what the pair trades at, so the card is useful at a glance.
            if let (Some((pool, pay0)), Some(to)) = (&pair, &card.to) {
                let rate = pool.spot_price().map(|p| if *pay0 { p } else { 1.0 / p });
                if let Some(r) = rate {
                    q_lines.push(kv(
                        t,
                        "rate",
                        Span::styled(format!("1 {} ≈ {} {}", card.from.symbol(), fmt_price(r), to.symbol()), t.strong_style()),
                    ));
                    q_lines.push(kv(
                        t,
                        "inverse",
                        Span::styled(format!("1 {} ≈ {} {}", to.symbol(), fmt_price(1.0 / r), card.from.symbol()), t.text_style()),
                    ));
                }
                let change = pool.change_24h().map(|c| if *pay0 { c } else { (100.0 / (100.0 + c) - 1.0) * 100.0 });
                q_lines.push(kv(t, "24h", pct_span(t, change)));
                q_lines.push(kv(
                    t,
                    "pool",
                    Span::styled(
                        format!(
                            "{} · {} TVL",
                            short_address(&pool.address),
                            pool.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into())
                        ),
                        t.dim_style(),
                    ),
                ));
                q_lines.push(Line::from(""));
            }
            q_lines.push(Line::from(Span::styled("Type an amount; the quote updates as you type.", t.dim_style())));
            q_lines.push(Line::from(""));
            q_lines.push(Line::from(Span::styled("Quotes read the Quainance router and pools on-chain.", t.dim_style())));
            q_lines
                .push(Line::from(Span::styled("Token inputs need an exact approval first; each step is its own review.", t.dim_style())));
        }
    }
    let _ = network;
    f.render_widget(Paragraph::new(q_lines).wrap(Wrap { trim: true }), inner);
}

/// `25%  50%  75%  max` under the amount, the last share picked lit.
fn preset_line(t: &Theme, preset: Option<u8>) -> Line<'static> {
    let mut spans = vec![Span::raw(" ".repeat(14))];
    for (share, label) in [(25u8, "25%"), (50, "50%"), (75, "75%"), (100, "max")] {
        let style = if preset == Some(share) { t.strong_style().fg(t.focus).add_modifier(Modifier::REVERSED) } else { t.dim_style() };
        spans.push(Span::styled(format!(" {label} "), style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled("  % · f ↕", t.dim_style()));
    Line::from(spans)
}

/// The swap pair's hourly chart, pay token priced in the receive token.
fn draw_swap_chart(f: &mut Frame, app: &App, t: &Theme, area: Rect, pair: Option<&(wallet_core::markets::Pool, bool)>) {
    let card = &app.eco.swap;
    let to = card.to.as_ref().map(|a| a.symbol()).unwrap_or_default();
    let title = format!("{}/{to} · 1h", card.from.symbol());
    let block = panel(t, &title, false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some((pool, pay0)) = pair else {
        let loading = app.eco.markets_view.pools_loading || app.eco.markets_view.pools.is_none();
        if loading {
            empty_state(f, inner, t, spinner(), "Reading the market…", &[]);
        } else {
            empty_state(f, inner, t, "○", "No pool trades this pair directly; the router finds a path through WQUAI.", &[]);
        }
        return;
    };
    let chart_w = inner.width.saturating_sub(11).max(8);
    let step = if chart_w as usize / 2 >= 24 { 2 } else { 1 };
    let n = ((chart_w / step) as usize).min(super::eco::MARKET_CANDLES);
    let cs = app.chart_candles(pool, *pay0, super::eco::SWAP_CHART_BUCKET, n);
    if cs.is_empty() {
        empty_state(f, inner, t, spinner(), "Reading the pair's history…", &[]);
    } else {
        draw_candles(f, t, inner, &cs, step, super::eco::SWAP_CHART_BUCKET);
    }
}

/// QUAI ⇄ Qi has two markets: the protocol conversion (one transaction, the controller's rate,
/// output locked for weeks) and the market route through Quainance (wrap, swap, unwrap: several
/// transactions and LP costs, spendable in minutes). Both are quoted for the amount on the card
/// and either can be started here.
pub fn draw_convert_card(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let card = &app.eco.convert;
    let [left, right] = if area.width < 100 {
        Layout::vertical([Constraint::Length(12), Constraint::Min(8)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(area)
    };
    let (pay, get) = if card.qi_to_quai { ("Qi", "QUAI") } else { ("QUAI", "Qi") };
    let avail = if card.qi_to_quai {
        app.dash.qi.as_ref().map(|q| format!("{} Qi", amount::qi(q.balance.spendable)))
    } else {
        app.dash.accounts.first().map(|a| format!("{} QUAI", amount::format_amount_short(a.balance, 18, 4)))
    };
    // Zero means the user has not chosen and no quote has landed yet; the quote's suggestion takes
    // over as soon as one does (see the Ev::Quote arm).
    let slippage = match (card.slippage_bps, card.quote.as_ref()) {
        (0, Some(q)) => q.suggested_slippage_bps,
        (0, None) => 300,
        (set, _) => set,
    };
    let comparison = match &card.routes {
        Some(Ok(c)) if c.direction.as_str() == if card.qi_to_quai { "qi_to_quai" } else { "quai_to_qi" } => Some(c),
        _ => None,
    };
    let route_name = if card.market { "market route (wrap · swap · unwrap)" } else { "protocol conversion" };
    let mut lines = vec![
        card_row(
            t,
            card.field == 0,
            "direction",
            vec![
                images::native_span(app, t, pay),
                Span::styled(format!(" {pay} → "), t.strong_style().fg(t.focus)),
                images::native_span(app, t, get),
                Span::styled(format!(" {get} ‹›"), t.strong_style().fg(t.focus)),
            ],
        ),
        card_row(
            t,
            card.field == 1,
            "you pay",
            vec![
                amount_span(t, &card.amount, card.field == 1),
                Span::styled(format!(" {pay}"), t.dim_style()),
                Span::styled(avail.map(|a| format!("   available {a}")).unwrap_or_default(), t.dim_style()),
            ],
        ),
        card_row(
            t,
            card.field == 2,
            "slippage",
            vec![Span::styled(format!("{:.2}% ‹› (conversion)", f64::from(slippage) / 100.0), t.text_style())],
        ),
        card_row(t, card.field == 3, "route", vec![Span::styled(format!("{route_name} ‹›"), t.strong_style().fg(t.focus))]),
        Line::from(""),
    ];
    let ready = comparison.is_some_and(|c| if card.market { c.market.usable() } else { c.protocol.usable() });
    lines.push(Line::from(vec![
        key(t, "enter"),
        Span::raw(match (card.market, ready) {
            (true, true) => "start the market route",
            (true, false) => "quoting…",
            (false, _) if card.quote.is_some() => "review the conversion",
            (false, _) => "quote the conversion",
        }),
        Span::styled("   r route   f flip", t.dim_style()),
    ]));
    if card.market {
        lines.push(Line::from(Span::styled("Each step is reviewed on its own; the next opens when the previous confirms.", t.dim_style())));
    } else {
        lines.push(Line::from(Span::styled(
            "Conversions in one prime block share a discount; beyond your slippage they refund (fee spent).",
            t.dim_style(),
        )));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "convert · QUAI ↔ Qi", true)), left);

    let block = panel(t, "both markets", false);
    let inner = block.inner(right);
    f.render_widget(block, right);
    let mut q = Vec::new();
    match (&card.routes, card.amount.is_empty()) {
        (_, true) => q.push(Line::from(Span::styled("Enter an amount to see both markets for it.", t.dim_style()))),
        (None, _) => q.push(Line::from(Span::styled(format!("{} reading both markets…", spinner()), t.dim_style()))),
        (Some(Err(e)), _) => q.push(Line::from(Span::styled(app::friendly_error(e), Style::default().fg(t.danger)))),
        (Some(Ok(c)), _) => {
            // Big quotes when both fit; one line each otherwise. Built twice at most: the big
            // version is measured against the panel before it is kept.
            let big = app.config.big_numbers && !app.plain;
            let mut lines = market_quotes(app, t, c, inner.width, big);
            if big && lines.len() as u16 > inner.height {
                lines = market_quotes(app, t, c, inner.width, false);
            }
            q.extend(lines);
        }
    }
    // What the conversion costs, once it has been quoted. Two different numbers, and the tolerance
    // has to clear both: what the node's own estimate says this loses right now, alone, and what a
    // shared block would discount. Either one above the slippage is a refund.
    if let Some(quote) = &card.quote
        && !card.market
        && inner.height > 10
    {
        let severity = |bps: u16| {
            if bps > slippage {
                t.danger
            } else if bps >= 500 {
                t.attention
            } else {
                t.ok
            }
        };
        q.push(Line::from(""));
        if let Some(bps) = quote.implied_slippage_bps.filter(|b| *b > 0) {
            q.push(Line::from(Span::styled("what it costs right now", t.strong_style())));
            q.push(Line::from(vec![
                Span::styled(format!("  {:<28} ", "discount at this instant"), t.dim_style()),
                Span::styled(wallet_core::ops::percent(bps), Style::default().fg(severity(bps))),
                Span::styled(if bps > slippage { "  above your slippage" } else { "" }, Style::default().fg(t.danger)),
            ]));
        }
        if quote.discount_saturated {
            // Four scenarios all reading 90% teach nothing; the way out does.
            for l in crate::tui::ui::textwrap(
                "The discount is at its floor: this size pays one tenth of the rate whatever slippage you set. Convert less at a time, or press r for the market route.",
                inner.width.saturating_sub(2).max(20) as usize,
            ) {
                q.push(Line::from(Span::styled(format!("  {l}"), Style::default().fg(t.danger))));
            }
        } else if inner.height > 14 {
            q.push(Line::from(Span::styled("refund risk if others share the block", t.strong_style())));
            for s in quote.scenarios.iter().take(inner.height.saturating_sub(14) as usize) {
                let risky = s.discount_bps > slippage;
                q.push(Line::from(vec![
                    Span::styled(format!("  {:<28} ", truncate(&s.label, 28)), t.dim_style()),
                    Span::styled(wallet_core::ops::percent(s.discount_bps), Style::default().fg(if risky { t.danger } else { t.ok })),
                    Span::styled(if risky { "  refunds" } else { "  clears" }, Style::default().fg(if risky { t.danger } else { t.ok })),
                ]));
            }
        }
        if let Some(h) = &quote.hold {
            for l in crate::tui::ui::textwrap(&h.note, inner.width.saturating_sub(2).max(20) as usize) {
                q.push(Line::from(Span::styled(format!("  {l}"), Style::default().fg(t.danger))));
            }
        }
    }
    // Untrimmed: the block digits' leading spaces are what line them up.
    f.render_widget(Paragraph::new(q).wrap(Wrap { trim: false }), inner);
}

/// The two markets' quotes, each marked with what it is: `◈` the protocol's own conversion, `≋`
/// the Quainance pools. With `big`, what each route pays is set in block digits so the two amounts
/// can be compared at a glance; the better one is green and ticked.
fn market_quotes(app: &App, t: &Theme, c: &wallet_core::qi_market::Comparison, width: u16, big: bool) -> Vec<Line<'static>> {
    let card = &app.eco.convert;
    let best = c.better().map(|r| r.name.clone());
    let mut q = Vec::new();
    for (market, route) in [(false, &c.protocol), (true, &c.market)] {
        let selected = market == card.market;
        let is_best = best.as_deref() == Some(route.name.as_str());
        let bar = Span::styled(if selected { "▌" } else { " " }, Style::default().fg(t.focus));
        let (icon, kind, colour) = if market { ("≋", "MARKET", t.quai) } else { ("◈", "PROTOCOL", t.qi) };
        let head = Style::default().fg(if selected { t.focus } else { t.text }).add_modifier(Modifier::BOLD);
        let amount_style = if is_best {
            Style::default().fg(t.ok).add_modifier(Modifier::BOLD)
        } else {
            t.strong_style().fg(if selected { t.focus } else { t.text })
        };
        let mut header = vec![
            bar.clone(),
            Span::styled(format!("{icon} "), Style::default().fg(colour).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{kind:<9}"), Style::default().fg(colour).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:<26}", truncate(&route.name, 26)), head),
        ];
        let shown = route.receives_display.clone().filter(|_| route.unavailable.is_none());
        // `12345.678 Qi` → whole `12,345`, fraction `.678`, unit `Qi`.
        let parts = shown.as_ref().map(|d| {
            let (number, unit) = d.rsplit_once(' ').unwrap_or((d.as_str(), ""));
            let (whole, frac) = number.split_once('.').map_or((number, String::new()), |(w, f)| (w, format!(".{f}")));
            (amount::group_thousands(whole), frac, unit.to_string())
        });
        let fits = parts.as_ref().is_some_and(|(whole, _, _)| hero_fits(width.saturating_sub(4), 5, &[whole.as_str()]));
        let tick = Span::styled(if is_best { "  ✓ pays more" } else { "" }, Style::default().fg(t.ok));
        match (&parts, big && fits) {
            (Some((whole, frac, unit)), true) => {
                header.push(tick);
                q.push(Line::from(header));
                for (i, row) in big_digits(whole).iter().enumerate() {
                    let mut spans = vec![bar.clone(), Span::raw("  "), Span::styled(row.clone(), amount_style)];
                    if i == 2 {
                        spans.push(Span::styled(frac.clone(), amount_style.remove_modifier(Modifier::BOLD)));
                        spans.push(Span::styled(format!(" {unit}"), t.dim_style()));
                    }
                    q.push(Line::from(spans));
                }
            }
            (Some((whole, frac, unit)), false) => {
                header.push(Span::raw(" "));
                header.push(Span::styled(format!("{whole}{frac} {unit}"), amount_style));
                header.push(tick);
                q.push(Line::from(header));
            }
            (None, _) => {
                header.push(Span::styled("   —", t.dim_style()));
                q.push(Line::from(header));
            }
        }
        match &route.unavailable {
            Some(why) => {
                q.push(Line::from(vec![bar.clone(), Span::styled(format!("  {}", truncate(why, 60)), Style::default().fg(t.danger))]))
            }
            None => {
                let steps: Vec<&str> = route.legs.iter().map(|l| l.label.as_str()).collect();
                // Each step already reads `wrap QUAI → WQUAI`, so steps are separated, not arrowed.
                q.push(Line::from(vec![bar.clone(), Span::styled(format!("  {}", steps.join("  ·  ")), t.dim_style())]));
                q.push(Line::from(vec![
                    bar.clone(),
                    Span::styled("  spendable ", t.dim_style()),
                    Span::styled(route.wait.clone(), t.text_style()),
                    Span::styled(format!("   {}", route.costs.join(" · ")), t.dim_style()),
                ]));
            }
        }
        for w in route.warnings.iter().take(3) {
            q.push(Line::from(vec![bar.clone(), Span::styled(format!("  ! {}", truncate(w, 62)), Style::default().fg(t.attention))]));
        }
        q.push(Line::from(""));
    }
    if let Some(bps) = c.market_advantage_bps {
        let (label, value) = if bps >= 0 { ("the market route pays", bps) } else { ("the protocol conversion pays", -bps) };
        q.push(Line::from(Span::styled(
            format!("{label} {}.{:02}% more at this block", value / 100, (value % 100).abs()),
            Style::default().fg(t.ok),
        )));
    }
    q.push(Line::from(Span::styled("Rates move with the pools and the block's conversion flow.", t.dim_style())));
    q
}

/// Quainance's launch zone: every launched token, newest first, with where it trades now. A token
/// still on its curve shows how far it is toward graduating; pooled ones trade from the Swap card.
pub fn draw_launches(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::launches::Phase;
    // The focused token's curve sits beside the list on wide screens, under it otherwise.
    let focused_bonding = app.launch_rows().get(app.selected).is_some_and(|l| l.phase == Phase::Bonding);
    let area = if focused_bonding && area.height >= 16 {
        let [list, curve] = if area.width >= 140 {
            Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area)
        } else {
            Layout::vertical([Constraint::Min(6), Constraint::Length(15)]).areas(area)
        };
        draw_curve(f, app, t, curve);
        list
    } else {
        area
    };
    let rows = app.launch_rows();
    let heading = match &app.eco.launches {
        Some(Ok(list)) => format!("launch zone · {} tokens", list.len()),
        _ => "launch zone".into(),
    };
    let block = panel(t, &heading, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    match &app.eco.launches {
        None => return empty_state(f, inner, t, spinner(), "Reading Quainance's launch zone…", &[]),
        Some(Err(e)) if rows.is_empty() => {
            return empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]);
        }
        _ if rows.is_empty() => {
            return empty_state(
                f,
                inner,
                t,
                "○",
                "No launches yet. New tokens start here on a bonding curve, then graduate to Markets.",
                &[("R", "reload")],
            );
        }
        _ => {}
    }
    let quai_usd = app.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
    let now = wallet_core::registry::now();
    // The name column and the age earn their place from 86 columns: below that the symbol and
    // numbers are what fit.
    let wide = inner.width >= 86;
    let height = inner.height.saturating_sub(2) as usize;
    let selected = app.selected.min(rows.len() - 1);
    let start = selected.saturating_sub(height.saturating_sub(2));
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "     {:<9} {}{:<17} {:>12} {:>13} {:>7}{}",
            "token",
            if wide { format!("{:<19}", "") } else { String::new() },
            "stage",
            "price QUAI",
            "USD",
            "trades",
            if wide { "   age" } else { "" }
        ),
        t.dim_style(),
    ))];
    for (i, l) in rows.iter().enumerate().skip(start).take(height) {
        let focused = i == selected;
        let style = if focused { t.selected() } else { t.text_style() };
        let stage = match (l.phase, l.progress_bps) {
            (Phase::Bonding, Some(bps)) => {
                let filled = (bps as usize * 10).div_ceil(10_000).min(10);
                format!("{}{} {:>3}%", "■".repeat(filled), "□".repeat(10 - filled), bps / 100)
            }
            (phase, _) => phase.text().to_string(),
        };
        let stage_colour = match l.phase {
            Phase::Bonding => t.attention,
            Phase::Graduated => t.ok,
            Phase::Pooled => t.focus,
            Phase::Other => t.dim,
        };
        let price = l.price_quai.map(super::views::fmt_price).unwrap_or_else(|| "—".into());
        let usd = l.price_quai.zip(quai_usd).map(|(p, u)| amount::usd_price(p * u)).unwrap_or_else(|| "—".into());
        // The token's own logo (Quainance's media proxy), or its monogram until that loads: the
        // launch zone is mostly tokens nobody has seen before, and a picture is how they are told
        // apart at a glance. The symbol beside it is still what identifies them.
        let mut spans = vec![
            Span::styled(if focused { "▌ " } else { "  " }, Style::default().fg(t.focus)),
            images::asset_span(app, t, &l.token, &l.symbol),
            Span::raw(" "),
            Span::styled(format!("{:<9} ", truncate(&l.symbol, 9)), style.add_modifier(Modifier::BOLD)),
        ];
        if wide {
            spans.push(Span::styled(format!("{:<18} ", truncate(&l.name, 18)), t.dim_style()));
        }
        spans.extend([
            Span::styled(format!("{stage:<17} "), Style::default().fg(stage_colour)),
            Span::styled(format!("{price:>12} "), t.text_style()),
            Span::styled(format!("{:>13} ", truncate(&usd, 13)), t.dim_style()),
            Span::styled(format!("{:>7}", l.trades()), t.dim_style()),
        ]);
        if wide {
            spans.push(Span::styled(format!("   {}", ago_short(l.created_at.min(now))), t.dim_style()));
        }
        // The selected row is one band across the panel, not a few highlighted cells.
        if focused {
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            spans.push(Span::raw(" ".repeat((inner.width as usize).saturating_sub(used))));
            let band = t.selected();
            spans = spans
                .into_iter()
                .map(|s| {
                    let st = s.style.patch(Style { bg: band.bg, ..Style::default() });
                    s.style(st)
                })
                .collect();
        }
        lines.push(Line::from(spans));
    }
    if let Some(selected) = rows.get(selected) {
        lines.push(Line::from(Span::styled(format!("price basis: {} · {}", selected.price_basis.label(), selected.token), t.dim_style())));
    }
    if let Some(Err(e)) = &app.eco.launches {
        lines.push(Line::from(Span::styled(
            format!("× last refresh failed: {}", truncate(&app::friendly_error(e), 60)),
            Style::default().fg(t.danger),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Where the focused token stands on its bonding curve: the price curve from launch to graduation,
/// the part already raised filled in, a marker at the current point, and what that means in
/// numbers — raised against the target, how much of the allocation is sold, the price now, and what
/// this wallet holds and is owed.
fn draw_curve(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rows = app.launch_rows();
    let Some(l) = rows.get(app.selected) else { return };
    let block = panel(t, &format!("{} on its bonding curve", l.symbol), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(m) = app.focused_curve().filter(|m| m.token == l.token) else {
        let text = match app.eco.curves.get(&l.token) {
            Some(Err(e)) => format!("× {}", truncate(&app::friendly_error(e), 70)),
            _ => format!("{} reading the curve…", spinner()),
        };
        f.render_widget(Paragraph::new(Span::styled(text, t.dim_style())), inner);
        return;
    };
    let quai_usd = app.eco.portfolio.as_ref().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
    let usd = |q: f64| quai_usd.map(|u| format!(" · {}", amount::usd_price(q * u))).unwrap_or_default();
    let mut lines = vec![
        Line::from(vec![
            Span::styled("raised  ", t.dim_style()),
            Span::styled(
                if m.target.is_zero() {
                    format!("{} QUAI · target unavailable", amount::compact(m.raised_quai()))
                } else {
                    format!("{} / {} QUAI", amount::compact(m.raised_quai()), amount::compact(m.target_quai()))
                },
                t.strong_style(),
            ),
            Span::styled(format!("  {:.1}% to graduation", m.progress_bps as f64 / 100.0), Style::default().fg(t.attention)),
        ]),
        Line::from(vec![
            Span::styled("sold    ", t.dim_style()),
            Span::raw(format!("{:.1}% of the curve's tokens", m.sold_bps() as f64 / 100.0)),
            Span::styled(format!("   fee {:.2}%", m.fee_bps as f64 / 100.0), t.dim_style()),
        ]),
        Line::from(vec![
            Span::styled(
                if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve) { "1 QUAI quote " } else { "price   " },
                t.dim_style(),
            ),
            Span::styled(format!("{} QUAI", fmt_price(m.spot_price)), t.strong_style()),
            Span::styled(usd(m.spot_price), t.dim_style()),
        ]),
    ];
    if !m.held.is_zero() {
        let held = amount::to_f64(m.held, m.token_decimals);
        lines.push(Line::from(vec![
            Span::styled("yours   ", t.dim_style()),
            Span::raw(format!("{} {} ≈ {} QUAI at this price", amount::compact(held), l.symbol, fmt_price(held * m.spot_price))),
        ]));
    }
    if !m.claimable.is_zero() {
        lines.push(Line::from(vec![
            Span::styled("credit  ", t.dim_style()),
            Span::styled(format!("{} QUAI to claim · c", amount::format_amount_short(m.claimable, 18, 4)), Style::default().fg(t.ok)),
        ]));
    }
    let text_h = lines.len() as u16;
    f.render_widget(Paragraph::new(lines), Rect { height: text_h.min(inner.height), ..inner });
    // The curve: one column per slice of the QUAI target, as tall as the price there.
    let chart = Rect { y: inner.y + text_h + 1, height: inner.height.saturating_sub(text_h + 3).min(24), ..inner };
    if chart.height < 3 || chart.width < 10 || m.points.is_empty() {
        return;
    }
    let max_price = m.points.iter().map(|p| p.1).fold(0.0, f64::max).max(f64::MIN_POSITIVE);
    let price_at = |raised: f64| {
        let i = m.points.partition_point(|p| p.0 < raised).min(m.points.len() - 1);
        m.points[i].1
    };
    let target = m.target_quai().max(f64::MIN_POSITIVE);
    let here = ((m.raised_quai() / target) * f64::from(chart.width - 1)).round() as u16;
    const EIGHTHS: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let buf = f.buffer_mut();
    for x in 0..chart.width {
        let raised = (f64::from(x) + 0.5) / f64::from(chart.width) * target;
        let eighths = ((price_at(raised) / max_price) * f64::from(chart.height) * 8.0).round() as u32;
        let colour = if x <= here { t.attention } else { t.dim };
        for row in 0..chart.height {
            let from_bottom = u32::from(chart.height - 1 - row);
            let fill = eighths.saturating_sub(from_bottom * 8).min(8) as usize;
            if fill > 0 {
                buf[(chart.x + x, chart.y + row)].set_symbol(EIGHTHS[fill]).set_fg(colour);
            }
        }
    }
    // The marker and the axis under the curve.
    let axis_y = chart.y + chart.height;
    let label = format!("▲ here · {} QUAI raised", amount::compact(m.raised_quai()));
    let label_x = here.min(chart.width.saturating_sub(label.chars().count() as u16));
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" ".repeat(label_x as usize), t.dim_style()),
            Span::styled(label, Style::default().fg(t.focus)),
        ])),
        Rect { x: chart.x, y: axis_y, width: chart.width, height: 1 },
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("launch", t.dim_style()),
            Span::styled(
                format!("{:>w$}", format!("graduates at {} QUAI", amount::compact(target)), w = chart.width.saturating_sub(6) as usize),
                t.dim_style(),
            ),
        ])),
        Rect { x: chart.x, y: axis_y + 1, width: chart.width, height: 1 },
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{} QUAI per {} at graduation · b buy  S sell  c claim", fmt_price(max_price), l.symbol),
            t.dim_style(),
        ))),
        Rect { x: chart.x, y: chart.y.saturating_sub(1), width: chart.width, height: 1 },
    );
}

pub fn draw_wrap_card(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let card = &app.eco.wrap;
    let [left, right] = if area.width < 100 {
        Layout::vertical([Constraint::Length(10), Constraint::Min(6)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area)
    };
    let (label, unit, story) = WRAP_MODES[card.mode];
    let mut lines = vec![card_row(t, card.field == 0, "mode", vec![Span::styled(format!("{label} ‹›"), t.strong_style().fg(t.focus))])];
    if card.mode != 1 {
        lines.push(card_row(
            t,
            card.field == 1,
            "amount",
            vec![amount_span(t, &card.amount, card.field == 1), Span::styled(format!(" {unit}"), t.dim_style())],
        ));
    }
    // What the selected mode can use: spendable Qi, unclaimed backing, whole WQI, or WQUAI.
    let w = app.dash.wrap.as_ref();
    let parse = |v: Option<&String>| v.and_then(|v| v.parse::<U256>().ok());
    let available = match card.mode {
        0 => app.dash.qi.as_ref().map(|q| format!("{} Qi spendable", amount::qi(q.balance.spendable))),
        1 => parse(w.and_then(|w| w.unclaimed_qits.as_ref())).map(|q| {
            if q.is_zero() {
                "nothing to claim yet (backing arrives after the wrap settles)".into()
            } else {
                format!("{} Qi ready to claim", amount::qi(q))
            }
        }),
        2 => w.and_then(|w| w.wqi_qi.clone()).map(|v| format!("{v} WQI (redeem whole Qi)")),
        3 => app.dash.accounts.first().map(|a| format!("{} QUAI", amount::format_amount_short(a.balance, 18, 4))),
        _ => parse(w.and_then(|w| w.wquai_atoms.as_ref())).map(|v| format!("{} WQUAI", amount::format_amount_short(v, 18, 4))),
    };
    if let Some(text) = available {
        lines.push(card_row(t, false, "available", vec![Span::styled(text, t.dim_style())]));
    }
    lines.push(Line::from(Span::styled(story, t.dim_style())));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![key(t, "enter"), Span::raw("review")]));
    if card.mode <= 1 {
        lines.push(step_line(t, &["wrap Qi", "wait for settlement", "claim WQI"], if card.mode == 0 { 0 } else { 2 }));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "wrap", true)), left);
    let mut s = Vec::new();
    match &app.dash.wrap {
        Some(w) => {
            s.push(kv(t, "account", Span::styled(short_address(&w.account), Style::default().fg(t.link))));
            let network = app.config.network(&app.network_id).ok();
            let token_icon = |address: Option<String>, symbol: &str| match address {
                Some(a) => images::asset_span(app, t, &a.to_lowercase(), symbol),
                None => Span::raw("  "),
            };
            s.push(kv_icon(
                t,
                "WQI",
                token_icon(network.as_ref().and_then(|n| n.wqi.clone()), "WQI"),
                Span::styled(w.wqi_qi.clone().map(|v| format!("{v} WQI")).unwrap_or_else(|| "—".into()), Style::default().fg(t.qi)),
            ));
            s.push(kv_icon(
                t,
                "unclaimed",
                images::native_span(app, t, "qi"),
                Span::styled(
                    w.unclaimed_qits
                        .clone()
                        .map(|v| format!("{} Qi", amount::qi(v.parse().unwrap_or_default())))
                        .unwrap_or_else(|| "—".into()),
                    Style::default().fg(t.attention),
                ),
            ));
            s.push(kv_icon(
                t,
                "WQUAI",
                token_icon(network.as_ref().and_then(|n| n.wquai.clone()), "WQUAI"),
                Span::styled(
                    w.wquai_atoms
                        .clone()
                        .map(|v| format!("{} WQUAI", amount::format_amount_short(v.parse().unwrap_or_default(), 18, 4)))
                        .unwrap_or_else(|| "—".into()),
                    Style::default().fg(t.quai),
                ),
            ));
        }
        None => s.push(Line::from(Span::styled(
            match &app.dash.wrap_error {
                Some(e) => format!("Wrapped balances unavailable: {}", app::friendly_error(e)),
                None => "Loading wrapped balances…".into(),
            },
            t.dim_style(),
        ))),
    }
    s.push(Line::from(""));
    for o in app.dash.ops.iter().filter(|o| o.kind.contains("wrap") || o.kind.contains("claim")).take(5) {
        s.push(Line::from(vec![
            Span::styled(format!("{} ", status_glyph(o.status)), t.dim_style()),
            Span::raw(truncate(&describe(o), 40)),
            Span::styled(format!("  {}", ago(o.created)), t.dim_style()),
        ]));
    }
    f.render_widget(Paragraph::new(s).block(panel(t, "wrapped balances", false)), right);
}

pub fn draw_token_picker(f: &mut Frame, app: &App, t: &Theme, area: Rect, query: &str, selected: usize, pay: bool) {
    use super::eco::{PickerEntry, RouteState};
    use wallet_core::swap::usd_compact;
    let entries = app.picker_entries(query, pay);
    let known: Vec<(String, String)> = entries
        .iter()
        .filter(|e| e.verified)
        .filter_map(|e| match &e.asset {
            SwapAsset::Token { address, symbol, .. } => Some((symbol.clone(), address.clone())),
            _ => None,
        })
        .collect();
    let mut lines = vec![
        Line::from(vec![Span::styled("› ", Style::default().fg(t.focus)), Span::styled(format!("{query}▏"), t.strong_style())]),
        Line::from(""),
    ];
    let height = area.height.saturating_sub(3) as usize;
    let start = selected.saturating_sub(height.saturating_sub(1));
    for (i, entry) in entries.iter().enumerate().skip(start).take(height) {
        let PickerEntry { asset, info, verified, holders, icon, route } = entry;
        let active = i == selected;
        let dead = matches!(route, RouteState::Dead);
        let (addr, lookalike) = match asset {
            SwapAsset::Quai => ("native".to_string(), false),
            SwapAsset::Token { address, symbol, .. } => {
                (short_address(address), !*verified && wallet_core::swap::lookalike_warning(symbol, address, &known).is_some())
            }
        };
        // An unreachable row stays visible so search can find it and say why, but it reads as
        // unavailable rather than merely unremarkable.
        let style = match (active, dead) {
            (true, _) => t.selected(),
            (false, true) => t.dim_style().bg(t.raised),
            (false, false) => t.text_style().bg(t.raised),
        };
        let badge = match asset {
            SwapAsset::Quai => images::native_span(app, t, "quai"),
            SwapAsset::Token { address, symbol, .. } => {
                images::badge_span(app, t, icon.clone().or_else(|| app.asset_icon_url(address)).as_deref(), symbol, address)
            }
        };
        // The route badge carries the hop count and the thinnest pool on the way: enough to tell
        // "this will cost you three fees" from "this pool holds four dollars".
        let route_span = match route {
            RouteState::Unknown => Span::raw(""),
            RouteState::Dead => Span::styled("  ✕ no route", Style::default().fg(t.danger)),
            RouteState::Fillable(info) if info.thin() => Span::styled(
                format!(
                    "  ⚠ {} hop{} · {} pool",
                    info.hops(),
                    if info.hops() == 1 { "" } else { "s" },
                    usd_compact(info.min_tvl_usd.unwrap_or(0.0))
                ),
                Style::default().fg(t.attention),
            ),
            RouteState::Fillable(info) if info.swap_count() > 1 => {
                Span::styled(format!("  2 swaps · {:.1}% fee", info.fee_bps() as f64 / 100.0), Style::default().fg(t.attention))
            }
            RouteState::Fillable(info) if info.hops() > 1 => {
                Span::styled(format!("  {} hops · {:.1}% fee", info.hops(), info.fee_bps() as f64 / 100.0), t.dim_style())
            }
            RouteState::Fillable(_) => Span::styled("  direct", Style::default().fg(t.ok)),
        };
        lines.push(Line::from(vec![
            Span::styled(if active { "▌" } else { " " }, Style::default().fg(t.focus)),
            badge,
            Span::raw(" "),
            Span::styled(format!("{:<10}", truncate(asset.symbol(), 10)), style.add_modifier(Modifier::BOLD)),
            if *verified {
                Span::styled(" ✓ ", Style::default().fg(t.ok))
            } else {
                Span::styled(" ⚠ ", Style::default().fg(t.attention))
            },
            Span::styled(format!("{addr:<14}"), t.dim_style()),
            Span::styled(format!("{:<16}", truncate(info, 16)), style),
            Span::styled(holders.map(|h| format!("{h} holders")).unwrap_or_default(), t.dim_style()),
            route_span,
            Span::styled(if lookalike { "  ⚠ lookalike symbol" } else { "" }, Style::default().fg(t.danger)),
        ]));
    }
    if entries.is_empty() {
        lines.push(Line::from(Span::styled("no matching tokens", t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), area);
    let _ = f;
}

// ---------------------------------------------------------------- NFTs

fn nft_tile(
    f: &mut Frame,
    app: &App,
    t: &Theme,
    rect: Rect,
    name: &str,
    image: Option<&str>,
    contract: &str,
    sub: Span<'static>,
    selected: bool,
) {
    let pic = Rect { height: rect.height.saturating_sub(2), ..rect };
    images::picture(app, f.buffer_mut(), pic, t, image, name, contract, true);
    let style = if selected { t.selected() } else { t.strong_style() };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(truncate(name, rect.width as usize), style))),
        Rect { y: rect.bottom().saturating_sub(2), height: 1, ..rect },
    );
    f.render_widget(Paragraph::new(Line::from(sub)), Rect { y: rect.bottom().saturating_sub(1), height: 1, ..rect });
}

pub fn draw_collected(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let title = match &app.eco.nfts {
        Some(Ok(v)) => format!("collected · {}", v.len()),
        _ => "collected".into(),
    };
    let block = panel(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    match &app.eco.nfts {
        None => empty_state(f, inner, t, spinner(), "Finding your NFTs and checking ownership on-chain…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry"), ("0 ]]", "data sources")]),
        Some(Ok(items)) if items.is_empty() => empty_state(
            f,
            inner,
            t,
            "◧",
            "No NFTs in this wallet yet. Browse collections, or what is for sale right now.",
            &[("3 ]", "explore"), ("3 ]]", "listings")],
        ),
        Some(Ok(items)) => {
            let (tile_w, tile_h) = if app.caps.tier == super::terminal::Tier::Text || app.plain { (22u16, 4u16) } else { (18u16, 10u16) };
            let cols = (inner.width / tile_w).max(1) as usize;
            *app.eco.grid_columns.borrow_mut() = cols;
            let rows_visible = (inner.height / tile_h).max(1) as usize;
            let row_of_selected = app.selected / cols;
            let first_row = row_of_selected.saturating_sub(rows_visible.saturating_sub(1));
            for (i, n) in items.iter().enumerate().skip(first_row * cols).take(rows_visible * cols) {
                let (row, col) = ((i / cols - first_row) as u16, (i % cols) as u16);
                let rect = Rect::new(inner.x + col * tile_w, inner.y + row * tile_h, tile_w.saturating_sub(2), tile_h.saturating_sub(1));
                let sub = match app.my_listing(&n.item.contract, &n.item.token_id) {
                    Some(l) => Span::styled(format!("◈ listed {}", app.listing_price(&l)), t.strong_style().fg(t.focus)),
                    None => Span::styled(
                        format!(
                            "✓ owned{}",
                            if n.kind == wallet_core::explorer::TokenKind::Erc1155 { format!(" ×{}", n.quantity) } else { String::new() }
                        ),
                        Style::default().fg(t.ok),
                    ),
                };
                nft_tile(f, app, t, rect, &n.item.name, n.item.image.as_deref(), &n.item.contract, sub, i == app.selected);
            }
        }
    }
}

pub fn draw_explore(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    // The marketplace's own totals, so the header says what the whole market did before the rows
    // say what each collection did.
    let days = app.eco.trade_window_days();
    let (market_volume, market_sales) = wallet_core::market::trade_window(&app.eco.nft_trades, days, wallet_core::registry::now());
    let listed: u64 = app.eco.nft_stats.values().filter_map(|c| c.active_listings).sum();
    let market = if app.eco.nft_trades.is_empty() && listed == 0 {
        String::new()
    } else {
        format!(
            " · {days}d {} QUAI over {market_sales} sale{} · {listed} listed",
            amount::group_thousands(&format!("{market_volume:.0}")),
            if market_sales == 1 { "" } else { "s" }
        )
    };
    let sorted = format!(" · by {}", app.eco.collection_sort.label());
    let title = match &app.eco.search {
        Some(q) => format!("collections · search: {q}▏"),
        None if !app.eco.search_text.is_empty() => format!("collections · “{}” · / to edit", app.eco.search_text),
        None => format!("collections{market}{sorted} · S sort · / search"),
    };
    // Wide enough for both: the directory says what exists, the tape beside it says what is
    // actually changing hands. Narrow terminals keep the directory at full width — a squeezed tape
    // would cost the collection names more than it adds.
    let (area, tape) = if area.width >= 124 && !app.eco.nft_trades.is_empty() {
        let [list, tape] = Layout::horizontal([Constraint::Min(70), Constraint::Length(46)]).areas(area);
        (list, Some(tape))
    } else {
        (area, None)
    };
    if let Some(tape) = tape {
        draw_market_activity(f, app, t, tape);
    }
    let block = panel(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    match &app.eco.collections {
        None => empty_state(f, inner, t, spinner(), "Loading the collections directory…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]),
        Some(Ok(_)) => {
            let list = app.eco.collections_filtered();
            if list.is_empty() {
                empty_state(f, inner, t, "◧", "No collections match.", &[("/", "search")]);
                return;
            }
            let row_h = if app.caps.tier == super::terminal::Tier::Text || app.plain { 1u16 } else { 2u16 };
            let visible = (inner.height.saturating_sub(1) / row_h).max(1) as usize;
            let offset = app.selected.saturating_sub(visible.saturating_sub(1));
            let wide = inner.width >= 104;
            let volume_header = app.eco.trade_window_label();
            let header = if wide {
                format!("{:<30}{:>12}{:>13}{:>8}{:>10}{:>9}", "collection", "floor", volume_header, "sales", "listed", "holders")
            } else {
                format!("{:<30}{:>12}{:>13}{:>9}", "collection", "floor", volume_header, "holders")
            };
            f.render_widget(
                Paragraph::new(Line::from(Span::styled(format!("{:<6}{header}", ""), t.dim_style()))),
                Rect { height: 1, ..inner },
            );
            for (i, c) in list.iter().enumerate().skip(offset).take(visible) {
                let y = inner.y + 1 + ((i - offset) as u16) * row_h;
                if row_h > 1 {
                    images::picture(app, f.buffer_mut(), Rect::new(inner.x, y, 4, 2), t, c.preview.as_deref(), &c.name, &c.address, true);
                }
                let style = if i == app.selected { t.selected() } else { t.text_style() };
                let stats = app.eco.nft_stats.get(&c.address.to_lowercase());
                // The indexer's floor is the marketplace's own cheapest ask; the explorer's is a
                // fallback for a collection it has not indexed. A floor priced in a token is
                // marked, because it cannot be compared with the QUAI ones beside it.
                let floor = match stats.and_then(|s| s.floor.map(|f| (f, s.floor_is_native()))) {
                    Some((f, true)) => format!("{} QUAI", amount::group_thousands(&format!("{f}"))),
                    Some((f, false)) => format!("{} tok", amount::group_thousands(&format!("{f}"))),
                    None => {
                        c.floor_quai.map(|q| format!("{} QUAI", amount::group_thousands(&format!("{q}")))).unwrap_or_else(|| "—".into())
                    }
                };
                let (vol7, sales7) = app.eco.nft_window(&c.address, days);
                let volume =
                    if sales7 == 0 { "—".to_string() } else { format!("{} QUAI", amount::group_thousands(&format!("{vol7:.0}"))) };
                let holders = stats.and_then(|s| s.holders).or(c.holders).map(|h| h.to_string()).unwrap_or_else(|| "—".into());
                let listed = stats.and_then(|s| s.active_listings).map(|l| l.to_string()).unwrap_or_else(|| "—".into());
                let quiet = t.dim_style();
                let mut spans = vec![
                    Span::styled(format!("{:<30}", truncate(&c.name, 28)), style.add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{floor:>12}"), t.text_style()),
                    Span::styled(format!("{volume:>13}"), if sales7 > 0 { Style::default().fg(t.ok) } else { quiet }),
                ];
                if wide {
                    spans.push(Span::styled(format!("{:>8}", if sales7 == 0 { "—".into() } else { sales7.to_string() }), quiet));
                    spans.push(Span::styled(format!("{listed:>10}"), t.text_style()));
                }
                spans.push(Span::styled(format!("{holders:>9}"), quiet));
                let line = Line::from(spans);
                f.render_widget(Paragraph::new(line), Rect::new(inner.x + 6, y, inner.width.saturating_sub(6), 1));
                if row_h > 1 {
                    f.render_widget(
                        Paragraph::new(Span::styled(short_address(&c.address), t.dim_style())),
                        Rect::new(inner.x + 6, y + 1, inner.width.saturating_sub(6), 1),
                    );
                }
            }
        }
    }
}

/// Listings as a table: a thumbnail (where pictures can be shown) beside price, item,
/// collection, market and age. Thumbnails come from the explorer's metadata when it has the item
/// (its resized media proxy), else the indexer's image.
fn draw_listings_table(f: &mut Frame, app: &App, t: &Theme, area: Rect, list: &[wallet_core::market::Listing], selected: Option<usize>) {
    let pictures = app.caps.tier != super::terminal::Tier::Text && !app.plain && app.config.images && area.width >= 96;
    let row_h: u16 = if pictures { 2 } else { 1 };
    let height = (area.height.saturating_sub(1) / row_h).max(1) as usize;
    let offset = selected.unwrap_or(0).saturating_sub(height.saturating_sub(1));
    let shown: Vec<(usize, &wallet_core::market::Listing)> = list.iter().enumerate().skip(offset).take(height).collect();
    let rows: Vec<Row> = shown
        .iter()
        .map(|&(i, l)| {
            let usd = app
                .eco
                .portfolio
                .as_ref()
                .and_then(|p| p.prices.as_ref())
                .and_then(|b| b.quai_usd)
                .filter(|_| l.is_native())
                .map(|p| amount::usd(amount::to_f64(l.price_amount(), 18) * p));
            let meta = app.eco.nft_meta.get(&(l.contract.to_lowercase(), l.token_id.clone())).and_then(|m| m.as_ref().ok());
            let name = l
                .name
                .clone()
                .or_else(|| meta.map(|m| m.name.clone()))
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("#{}", l.token_id));
            let collection = app.eco.collection_name(&l.contract);
            let item = if pictures {
                Cell::from(ratatui::text::Text::from(vec![
                    Line::from(truncate(&name, 28)),
                    Line::from(Span::styled(truncate(&collection, 28), t.dim_style())),
                ]))
            } else {
                Cell::from(truncate(&name, 22))
            };
            let mut cells = vec![
                Cell::from(
                    Line::from(match currency_span(app, t, l) {
                        Some(icon) => vec![icon, Span::raw(" "), Span::styled(app.listing_price(l), t.strong_style())],
                        None => vec![Span::styled(app.listing_price(l), t.strong_style())],
                    })
                    .alignment(Alignment::Right),
                ),
                Cell::from(Span::styled(usd.unwrap_or_default(), t.dim_style())),
                item,
                Cell::from(Span::styled(short_address(&l.contract), t.dim_style())),
                Cell::from(if l.buyable() {
                    Span::styled("zora", Style::default().fg(t.ok))
                } else {
                    Span::styled("seaport · view", t.dim_style())
                }),
                Cell::from(Span::styled(ago(l.created_at), t.dim_style())),
            ];
            if pictures {
                cells.insert(0, Cell::from(""));
            }
            let row = Row::new(cells).height(row_h);
            if Some(i) == selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let mut widths = vec![
        Constraint::Length(17),
        Constraint::Length(9),
        Constraint::Min(14),
        Constraint::Length(12),
        Constraint::Length(15),
        Constraint::Length(9),
    ];
    let mut header = vec!["price", "", "item", "collection", "market", "listed"];
    if pictures {
        widths.insert(0, Constraint::Length(4));
        header.insert(0, "");
    }
    f.render_widget(Table::new(rows, widths).column_spacing(1).header(Row::new(header).style(t.dim_style())), area);
    if pictures {
        for (k, &(_, l)) in shown.iter().enumerate() {
            app.eco.want_meta(&l.contract, &l.token_id);
            let rect = Rect::new(area.x, area.y + 1 + k as u16 * row_h, 4, 2);
            if rect.bottom() <= area.bottom() {
                let url = app.nft_image_url(&l.contract, &l.token_id);
                images::picture(app, f.buffer_mut(), rect, t, url.as_deref(), l.name.as_deref().unwrap_or(&l.token_id), &l.contract, true);
            }
        }
    }
}

pub fn draw_listings(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let scope = match &app.eco.listing_filter {
        Some(c) => app.eco.collection_name(c),
        None => "all collections".into(),
    };
    let scope = if app.eco.listings_mine { "yours".to_string() } else { scope };
    let title = format!("listings · Bazarr · {scope} · {}", app.eco.listing_sort.label());
    let block = panel(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.listings_mine {
        match &app.eco.my_listings {
            None => empty_state(f, inner, t, spinner(), "Loading your listings…", &[]),
            Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("m", "everyone's")]),
            Some(Ok(list)) if list.is_empty() => {
                empty_state(f, inner, t, "◧", "You have nothing listed.", &[("3", "collected · L to list"), ("m", "everyone's")])
            }
            Some(Ok(_)) => {
                let visible = app.eco.visible_listings();
                draw_listings_table(f, app, t, inner, &visible, Some(app.selected))
            }
        }
        return;
    }
    match app.eco.listings.get(&None) {
        None => empty_state(f, inner, t, spinner(), "Loading listings…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]),
        Some(Ok(list)) if list.is_empty() => empty_state(f, inner, t, "◧", "No active listings.", &[]),
        Some(Ok(_)) => {
            let visible = app.eco.visible_listings();
            draw_listings_table(f, app, t, inner, &visible, Some(app.selected))
        }
    }
}

fn draw_nft_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str, token_id: &str) {
    let meta = app.eco.nft_meta.get(&(contract.to_lowercase(), token_id.to_string()));
    let item = match meta {
        Some(Ok(i)) => Some(i.clone()),
        _ => match &app.eco.nfts {
            Some(Ok(v)) => v.iter().find(|n| n.item.contract == contract && n.item.token_id == token_id).map(|n| n.item.clone()),
            _ => None,
        },
    };
    let name = item.as_ref().map(|i| i.name.clone()).unwrap_or_else(|| format!("#{token_id}"));
    let full = area.width < 80;
    let [pic_area, info_area] = if full {
        Layout::vertical([Constraint::Length((area.height / 2).max(6)), Constraint::Min(6)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Length((area.height.saturating_sub(2) * 2).min(area.width / 2).max(16)), Constraint::Min(30)])
            .areas(area)
    };
    let block = panel(t, &name, true);
    let pinner = block.inner(pic_area);
    f.render_widget(block, pic_area);
    images::picture(app, f.buffer_mut(), pinner, t, item.as_ref().and_then(|i| i.image.as_deref()), &name, contract, true);
    let mut lines = Vec::new();
    if let Some(i) = &item {
        if !i.collection.is_empty() {
            lines.push(Line::from(Span::styled(i.collection.clone(), t.strong_style())));
        }
        if !i.description.is_empty() {
            lines.push(Line::from(Span::styled(truncate(&i.description, 240), t.dim_style())));
        }
        lines.push(Line::from(""));
    }
    lines.push(kv(t, "contract", Span::styled(contract.to_string(), Style::default().fg(t.link))));
    lines.push(kv(t, "token id", Span::raw(token_id.to_string())));
    let owned = matches!(&app.eco.nfts, Some(Ok(v)) if v.iter().any(|n| n.item.contract == contract && n.item.token_id == token_id));
    if owned {
        lines.push(kv(t, "owner", Span::styled("✓ you (checked on-chain)", Style::default().fg(t.ok))));
        if let Some(mine) = app.my_listing(contract, token_id) {
            let price = Span::styled(app.listing_price(&mine), t.strong_style().fg(t.focus));
            lines.push(match currency_span(app, t, &mine) {
                Some(icon) => kv_icon(t, "your listing", icon, price),
                None => kv(t, "your listing", price),
            });
            lines.push(Line::from(Span::styled("  L change price · X cancel · sells without asking you again", t.dim_style())));
        } else {
            lines.push(Line::from(Span::styled("  L list it for sale on Bazarr", t.dim_style())));
        }
    } else if let Some(Ok(check)) = app.eco.asks.get(&(contract.to_string(), token_id.to_string()))
        && let Some(o) = &check.owner
    {
        lines.push(kv(t, "owner", Span::raw(short_address(o))));
    }
    if let Some(c) = app.eco.collections.as_ref().and_then(|r| r.as_ref().ok()).and_then(|v| v.iter().find(|c| c.address == contract))
        && let Some(floor) = c.floor_quai
    {
        let usd = app
            .eco
            .portfolio
            .as_ref()
            .and_then(|p| p.prices.as_ref())
            .and_then(|b| b.quai_usd)
            .map(|p| format!(" ({})", amount::usd(floor * p)))
            .unwrap_or_default();
        lines.push(kv_icon(t, "floor", images::native_span(app, t, "quai"), Span::raw(format!("{floor} QUAI{usd} · reference only"))));
    }
    if let Some(l) = app.listing_for(contract, token_id) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("listing", t.strong_style())));
        let price = Span::styled(app.listing_price(&l), t.strong_style());
        lines.push(match currency_span(app, t, &l) {
            Some(icon) => kv_icon(t, "price", icon, price),
            None => kv(t, "price", price),
        });
        lines.push(kv(
            t,
            "market",
            Span::raw(if l.buyable() { "Zora ask (buy in wallet: b)".to_string() } else { "Seaport · view on Bazarr (o)".to_string() }),
        ));
        match app.eco.asks.get(&(contract.to_string(), token_id.to_string())) {
            None => lines.push(kv(t, "on-chain", Span::styled(format!("{} checking…", spinner()), t.dim_style()))),
            Some(Err(e)) => lines.push(kv(t, "on-chain", Span::styled(truncate(e, 60), Style::default().fg(t.danger)))),
            Some(Ok(check)) if check.valid => {
                lines.push(kv(t, "on-chain", Span::styled("✓ ask valid · seller owns it", Style::default().fg(t.ok))));
                let mut steps = Vec::new();
                if check.buyer_module_approval_needed {
                    steps.push("approve Zora module (once)");
                }
                if check.buyer_token_approval_needed {
                    steps.push("approve payment token");
                }
                steps.push("buy");
                lines.push(step_line(t, &steps, 0));
            }
            Some(Ok(check)) => {
                for p in &check.problems {
                    lines.push(Line::from(Span::styled(format!("× {p}"), Style::default().fg(t.danger))));
                }
            }
        }
    }
    if let Some(i) = &item
        && !i.traits.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("traits", t.strong_style())));
        for (k, v) in i.traits.iter().take(12) {
            lines.push(Line::from(vec![
                Span::styled(format!("  {:<14}", truncate(&k.to_lowercase(), 14)), t.dim_style()),
                Span::raw(v.clone()),
            ]));
        }
    }
    if meta.is_none() && item.is_none() {
        lines.push(Line::from(Span::styled(format!("{} loading metadata…", spinner()), t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "detail", false)), info_area);
}

fn draw_collection_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str) {
    let [grid_area, bottom] = Layout::vertical([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(area);
    // Sales sit beside the listings where there is width for both: what it sold for, next to what
    // it is asking.
    let (listing_area, sales_area) = if bottom.width >= 120 {
        let [l, s] = Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(bottom);
        (l, Some(s))
    } else {
        (bottom, None)
    };
    if let Some(sales) = sales_area {
        draw_collection_sales(f, app, t, sales, contract);
    }
    let focused = app.eco.collection_listings_focused;
    let stats = app.eco.nft_stats.get(&contract.to_lowercase());
    let window_days = app.eco.trade_window_days();
    let (vol7, sales7) = app.eco.nft_window(contract, window_days);
    let mut parts: Vec<String> = Vec::new();
    if let Some(s) = stats {
        if let Some(floor) = s.floor {
            let unit = if s.floor_is_native() { "QUAI" } else { "tok" };
            parts.push(format!("floor {} {unit}", amount::group_thousands(&format!("{floor}"))));
        }
        if let Some(v) = s.volume_quai.filter(|v| *v > 0.0) {
            parts.push(format!("all-time {} QUAI", amount::group_thousands(&format!("{v:.0}"))));
        }
        if let Some(l) = s.active_listings {
            parts.push(format!("{l} listed"));
        }
        if let Some(h) = s.holders {
            parts.push(format!("{h} holders"));
        }
        if let Some(supply) = s.total_supply {
            parts.push(format!("{supply} items"));
        }
    }
    if sales7 > 0 {
        parts.insert(0, format!("{window_days}d {} QUAI over {sales7}", amount::group_thousands(&format!("{vol7:.0}"))));
    }
    let title = if parts.is_empty() { "items".to_string() } else { format!("items · {}", parts.join(" · ")) };
    let block = panel(t, &title, !focused);
    let inner = block.inner(grid_area);
    f.render_widget(block, grid_area);
    match app.eco.collection_items.get(contract) {
        None => empty_state(f, inner, t, spinner(), "Loading items…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[]),
        Some(Ok(items)) if items.is_empty() => empty_state(f, inner, t, "◧", "No items indexed.", &[]),
        Some(Ok(items)) => {
            let (tile_w, tile_h) = if app.caps.tier == super::terminal::Tier::Text || app.plain { (22u16, 4u16) } else { (16u16, 9u16) };
            let cols = (inner.width / tile_w).max(1) as usize;
            let rows_visible = (inner.height / tile_h).max(1) as usize;
            let first_row = (app.detail_selected / cols).saturating_sub(rows_visible.saturating_sub(1));
            for (i, it) in items.iter().enumerate().skip(first_row * cols).take(rows_visible * cols) {
                let (row, col) = ((i / cols - first_row) as u16, (i % cols) as u16);
                let rect = Rect::new(inner.x + col * tile_w, inner.y + row * tile_h, tile_w.saturating_sub(2), tile_h.saturating_sub(1));
                let listed = app
                    .listing_for(contract, &it.token_id)
                    .map(|l| Span::styled(app.listing_price(&l), Style::default().fg(t.focus)))
                    .unwrap_or_else(|| Span::styled(format!("#{}", it.token_id), t.dim_style()));
                nft_tile(f, app, t, rect, &it.name, it.image.as_deref(), contract, listed, !focused && i == app.detail_selected);
            }
        }
    }
    let title = if focused { "active listings · j/k select · b buy · enter open" } else { "active listings · tab to select" };
    let block = panel(t, title, focused);
    let inner = block.inner(listing_area);
    f.render_widget(block, listing_area);
    match app.eco.listings.get(&Some(contract.to_string())) {
        None => empty_state(f, inner, t, spinner(), "Loading listings…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[]),
        Some(Ok(list)) if list.is_empty() => empty_state(f, inner, t, "○", "Nothing listed right now.", &[]),
        Some(Ok(list)) => {
            let selected = focused.then_some(app.eco.collection_listing.min(list.len() - 1));
            draw_listings_table(f, app, t, inner, list, selected)
        }
    }
}

/// What a collection has actually sold for: a price line over its sales, then the recent ones.
/// Only sales paid in QUAI are drawn, because a line mixing currencies would say nothing.
/// Every sale the marketplace has seen, newest first — the NFT counterpart of the DEX tape.
///
/// Market-wide rather than filtered to the selected collection: a collection's own sales already
/// have a panel on its detail view, and what this answers is the question the directory cannot,
/// which is what is moving right now. Sales in a token are marked rather than converted, so a
/// column of QUAI prices is never a mixed sum.
fn draw_market_activity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let block = panel(t, &format!("recent buys · {}", app.eco.nft_trades.len()), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.nft_trades.is_empty() {
        return empty_state(f, inner, t, "○", "No sales through the marketplace yet.", &[]);
    }
    let rows: Vec<Row> = app
        .eco
        .nft_trades
        .iter()
        .take(inner.height.saturating_sub(1) as usize)
        .map(|s| {
            let item = s.name.clone().unwrap_or_else(|| format!("#{}", s.token_id));
            let price = match (s.price_quai, s.is_native()) {
                (Some(p), true) => format!("{} QUAI", amount::group_thousands(&format!("{p}"))),
                (Some(p), false) => format!("{} tok", amount::group_thousands(&format!("{p}"))),
                (None, _) => "—".into(),
            };
            // A sale this wallet was on either side of is worth spotting in the tape.
            let buyer = if mine.contains(&s.buyer.to_lowercase()) {
                Span::styled("you", t.strong_style().fg(t.focus))
            } else if mine.contains(&s.seller.to_lowercase()) {
                Span::styled("sold", t.strong_style().fg(t.focus))
            } else {
                Span::styled(short_address(&s.buyer), t.dim_style())
            };
            Row::new(vec![
                Cell::from(Span::styled(super::ui::ago_short(s.at), t.dim_style())),
                Cell::from(Span::styled(truncate(&item, 16), t.text_style())),
                Cell::from(Line::from(Span::styled(price, Style::default().fg(t.ok))).alignment(Alignment::Right)),
                Cell::from(buyer),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Length(4), Constraint::Min(10), Constraint::Length(13), Constraint::Length(10)])
            .column_spacing(2)
            .header(Row::new(["", "item", "price", "buyer"]).style(t.dim_style())),
        inner,
    );
}

fn draw_collection_sales(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str) {
    let c = contract.to_lowercase();
    let sales: Vec<&wallet_core::market::Trade> = app.eco.nft_trades.iter().filter(|x| x.contract == c).collect();
    let block = panel(t, &format!("sales · {}", sales.len()), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if sales.is_empty() {
        return empty_state(f, inner, t, "○", "No sales through the marketplace yet.", &[]);
    }
    // Oldest first for the line; the list below stays newest first.
    let mut priced: Vec<f64> = sales.iter().filter(|x| x.is_native()).filter_map(|x| x.price_quai).collect();
    priced.reverse();
    let list_area = if inner.height >= 7 && priced.len() >= 2 {
        let [chart, list] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(inner);
        let scaled: Vec<u64> = priced.iter().map(|p| (p * 100.0).round().max(0.0) as u64).collect();
        let low = priced.iter().copied().fold(f64::INFINITY, f64::min);
        let high = priced.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        f.render_widget(
            ratatui::widgets::Sparkline::default().data(&scaled).style(Style::default().fg(t.ok)).block(
                ratatui::widgets::Block::default().title(Span::styled(
                    format!(" {} – {} QUAI ", amount::group_thousands(&format!("{low}")), amount::group_thousands(&format!("{high}"))),
                    t.dim_style(),
                )),
            ),
            chart,
        );
        list
    } else {
        inner
    };
    let now = wallet_core::registry::now();
    let rows: Vec<Row> = sales
        .iter()
        .take(list_area.height as usize)
        .map(|s| {
            let price = match (s.price_quai, s.is_native()) {
                (Some(p), true) => format!("{} QUAI", amount::group_thousands(&format!("{p}"))),
                (Some(p), false) => format!("{} tok", amount::group_thousands(&format!("{p}"))),
                (None, _) => "—".into(),
            };
            let item = s.name.clone().unwrap_or_else(|| format!("#{}", s.token_id));
            Row::new(vec![
                Cell::from(Span::styled(truncate(&item, 18), t.text_style())),
                Cell::from(Span::styled(price, Style::default().fg(t.ok))),
                Cell::from(Span::styled(s.kind_label().to_string(), t.dim_style())),
                Cell::from(Span::styled(super::ui::ago(now.saturating_sub(s.at)), t.dim_style())),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Min(10), Constraint::Length(16), Constraint::Length(9), Constraint::Length(7)]),
        list_area,
    );
}

fn draw_activity_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, key: &str) {
    let block = panel(t, "activity detail", true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let network = app.config.network(&app.network_id).ok();
    let mut lines = Vec::new();
    if let Some(id) = key.strip_prefix("op:")
        && let Some(op) = app.dash.ops.iter().find(|o| o.id == id)
    {
        lines.push(Line::from(Span::styled(describe(op), t.strong_style())));
        lines.push(Line::from(Span::styled(format!("{} {}", status_glyph(op.status), op.status.as_str()), t.text_style())));
        lines.push(Line::from(""));
        lines.extend(op_timeline(t, op));
        lines.push(Line::from(""));
        lines.push(kv(t, "operation", Span::raw(op.id.clone())));
        lines.push(kv(t, "from", Span::raw(op.account.clone())));
        lines.push(kv(t, "to", Span::raw(op.counterparty.clone())));
        for (label, value) in super::ui::cost_lines(app, t, Some(op), None) {
            lines.push(kv(t, label, value));
        }
        if let Some(h) = &op.tx_hash {
            lines.push(kv(t, "tx", Span::raw(h.clone())));
            if let Some(url) = network.as_ref().and_then(|n| n.tx_url(h)) {
                lines.push(kv(t, "explorer", Span::styled(url, Style::default().fg(t.link))));
            }
        }
        if let Some(obj) = op.detail.as_object() {
            for (k, v) in obj.iter().filter(|(k, _)| !matches!(k.as_str(), "native_value" | wallet_core::appdb::TIMELINE)).take(14) {
                lines.push(kv(
                    t,
                    k,
                    Span::styled(truncate(&v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()), 90), t.dim_style()),
                ));
            }
        }
    } else if let Some(k) = key.strip_prefix("act:")
        && let Some(a) = app.dash.activity.iter().find(|a| a.key == k)
    {
        lines.push(Line::from(Span::styled(activity_text(a), t.strong_style())));
        lines.push(Line::from(""));
        lines.push(kv(t, "account", Span::raw(a.address.clone())));
        if let Some(cp) = a.detail["counterparty"].as_str() {
            lines.push(kv(t, if a.direction == "in" { "from" } else { "to" }, Span::raw(cp.to_string())));
        }
        if let Some(token) = a.detail["token"].as_str() {
            lines.push(kv(t, "token", Span::raw(token.to_string())));
        }
        for (label, value) in super::ui::cost_lines(app, t, None, Some(a)) {
            lines.push(kv(t, label, value));
        }
        if let Some(h) = &a.tx_hash {
            lines.push(kv(t, "tx", Span::raw(h.clone())));
            if let Some(url) = network.as_ref().and_then(|n| n.tx_url(h)) {
                lines.push(kv(t, "explorer", Span::styled(url, Style::default().fg(t.link))));
            }
        }
        if let Some(b) = a.block {
            lines.push(kv(t, "block", Span::raw(amount::group_thousands(&b.to_string()))));
        }
        lines.push(kv(t, "source", Span::styled(a.detail["source"].as_str().unwrap_or("node").to_string(), t.dim_style())));
    } else {
        lines.push(Line::from(Span::styled("this row is no longer in the recent list", t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

/// One operation's life, stage by stage: when each happened and how long it took from the one
/// before; an open operation ends on what it is waiting for.
fn op_timeline(t: &Theme, op: &wallet_core::appdb::Operation) -> Vec<Line<'static>> {
    use chrono::TimeZone;
    let recorded: Vec<(String, u64, Option<String>)> = op.detail[wallet_core::appdb::TIMELINE]
        .as_array()
        .map(|list| {
            list.iter()
                .filter_map(|s| Some((s["s"].as_str()?.to_string(), s["at"].as_u64()?, s["tx"].as_str().map(str::to_string))))
                .collect()
        })
        .unwrap_or_default();
    // Journals from before timelines were kept: what is known is when it began and where it is.
    let stages = if recorded.is_empty() {
        let mut v = vec![("created".to_string(), op.created, None)];
        if op.updated > op.created {
            v.push((op.status.as_str().to_string(), op.updated, None));
        }
        v
    } else {
        recorded
    };
    let clock = |at: u64| {
        chrono::Local
            .timestamp_opt(at as i64, 0)
            .single()
            .map(|d| {
                let old = wallet_core::registry::now().saturating_sub(at) > 86_400;
                d.format(if old { "%b %d %H:%M:%S" } else { "%H:%M:%S" }).to_string()
            })
            .unwrap_or_default()
    };
    let mut lines = vec![Line::from(Span::styled("timeline", t.strong_style()))];
    let mut prev: Option<u64> = None;
    for (stage, at, tx) in &stages {
        let color = match stage.as_str() {
            "confirmed" | "settled" => t.ok,
            "failed" | "cancelled" => t.danger,
            "replaced" | "refunded" | "unknown" => t.attention,
            _ => t.pending,
        };
        let gap = prev.map(|p| format!("+{}", wallet_core::track::human_duration(at.saturating_sub(p).max(1)))).unwrap_or_default();
        let mut spans = vec![
            Span::styled("  ● ", Style::default().fg(color)),
            Span::styled(format!("{stage:<11}"), Style::default().fg(color)),
            Span::styled(format!("{:<16}", clock(*at)), t.text_style()),
            Span::styled(format!("{gap:<8}"), t.dim_style()),
        ];
        if let Some(tx) = tx {
            spans.push(Span::styled(format!("now {}", short_address(tx)), t.dim_style()));
        }
        lines.push(Line::from(spans));
        prev = Some(*at);
    }
    if !op.status.is_terminal() {
        let waiting = match op.status {
            wallet_core::appdb::OpStatus::Prepared => "waiting for your review",
            wallet_core::appdb::OpStatus::Signed | wallet_core::appdb::OpStatus::Submitted => "waiting for a block",
            wallet_core::appdb::OpStatus::Settling | wallet_core::appdb::OpStatus::Locked => "waiting for it to settle",
            _ => "waiting",
        };
        let since = prev.map(|p| wallet_core::track::human_duration(wallet_core::registry::now().saturating_sub(p).max(1)));
        lines.push(Line::from(vec![
            Span::styled(format!("  {} ", spinner()), Style::default().fg(t.pending)),
            Span::styled(format!("{waiting}{}", since.map(|s| format!(" · {s}")).unwrap_or_default()), t.dim_style()),
        ]));
    }
    lines
}

// ---------------------------------------------------------------- System › Data sources

pub fn draw_data_sources(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let [list, status] = Layout::vertical([Constraint::Length(app::DATA_SOURCES.len() as u16 + 2), Constraint::Min(4)]).areas(area);
    let c = &app.config;
    let offline = wallet_core::http::offline();
    let on_off = |b: bool| {
        if offline {
            "○ off (--offline-data)".to_string()
        } else if b {
            "● on".into()
        } else {
            "○ off".into()
        }
    };
    let rows: Vec<Row> = app::DATA_SOURCES
        .iter()
        .enumerate()
        .map(|(i, (id, label))| {
            let value = match *id {
                "explorer_lookups" => on_off(c.explorer_lookups),
                "market_data" => on_off(c.fetch_prices),
                "images" => on_off(c.images),
                "token_icons" => on_off(c.token_icons),
                _ => {
                    if app.eco.testing {
                        format!("{} testing…", spinner())
                    } else {
                        "›".into()
                    }
                }
            };
            let row = Row::new(vec![Cell::from(*label), Cell::from(Span::styled(value, Style::default().fg(t.focus)))]);
            if i == app.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    let block = panel(t, "data sources", true);
    let inner = block.inner(list);
    f.render_widget(block, list);
    f.render_widget(Table::new(rows, [Constraint::Length(46), Constraint::Min(16)]), inner);
    let network = app.config.network(&app.network_id).ok();
    let mut lines = Vec::new();
    if let Some(n) = &network {
        let ex = wallet_core::explorer::Explorer::for_network(n);
        lines.push(kv(t, "network", Span::raw(n.name.clone())));
        lines.push(kv(
            t,
            "backend",
            Span::raw(match ex.backend {
                wallet_core::explorer::Backend::Quai => format!("explorer.qu.ai API · {}", ex.base),
                wallet_core::explorer::Backend::Blockscout => format!("Blockscout · {}", ex.base),
                wallet_core::explorer::Backend::ChainOnly => "chain only (no explorer)".into(),
            }),
        ));
        lines.push(kv(
            t,
            "swaps",
            Span::raw(
                n.ecosystem
                    .quainance_router
                    .as_ref()
                    .map(|r| format!("Quainance {}", short_address(&r.address)))
                    .unwrap_or_else(|| "—".into()),
            ),
        ));
        lines.push(kv(t, "marketplace", Span::raw(n.ecosystem.bazarr_indexer.clone().unwrap_or_else(|| "—".into()))));
        for content in [wallet_core::ipfs::Content::Abi, wallet_core::ipfs::Content::Media] {
            let gateway = wallet_core::ipfs::gateway(content);
            let label = match content {
                wallet_core::ipfs::Content::Abi => "IPFS · ABIs",
                wallet_core::ipfs::Content::Media => "IPFS · images",
            };
            lines.push(kv(
                t,
                label,
                if gateway.is_local() {
                    Span::styled(format!("{} · your node, reached directly", gateway.display()), Style::default().fg(t.ok))
                } else if gateway.is_default_for(content) {
                    Span::styled(format!("{} · Quai's gateway, the default for {}", gateway.display(), content.label()), t.dim_style())
                } else {
                    Span::raw(format!(
                        "{} · {}",
                        gateway.display(),
                        if wallet_core::http::proxy().is_some() { "through the proxy" } else { "direct" }
                    ))
                },
            ));
        }
        lines.push(kv(
            t,
            "proxy",
            match wallet_core::http::proxy() {
                Some(p) => Span::styled(format!("{p} · lookups only; node RPC goes direct"), Style::default().fg(t.ok)),
                None => Span::styled("none · `config set proxy socks5h://127.0.0.1:9050` for Tor", t.dim_style()),
            },
        ));
    }
    lines.push(Line::from(Span::styled(
        "Explorer lookups send your addresses (and IP) to the explorer. Market data and images do not include your addresses.",
        t.dim_style(),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("request budget · this process, and the server's shared window", t.strong_style())));
    let hosts = wallet_core::http::host_statuses();
    if hosts.is_empty() {
        lines.push(Line::from(Span::styled("  no requests yet", t.dim_style())));
    }
    for h in hosts {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<24}", h.host), t.text_style()),
            Span::styled(format!("{:>3} last min · {} total", h.last_minute, h.total), t.dim_style()),
            match h.server {
                Some((limit, remaining, reset)) => Span::styled(
                    format!("  · server {remaining}/{limit} left, resets {reset}s"),
                    if remaining * 5 < limit { Style::default().fg(t.attention) } else { t.dim_style() },
                ),
                None => Span::styled(format!("  · paced {}/min", h.per_minute), t.dim_style()),
            },
            Span::styled(
                if h.rate_limited > 0 { format!("  · {}× 429", h.rate_limited) } else { String::new() },
                Style::default().fg(t.attention),
            ),
            Span::styled(
                h.last_error.map(|(at, e)| format!("   last error {} · {}", ago(at), truncate(&e, 40))).unwrap_or_default(),
                Style::default().fg(t.danger),
            ),
        ]));
    }
    if let Some(results) = &app.eco.test {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("connection test", t.strong_style())));
        for (name, r, ms) in results {
            match r {
                Ok(d) => lines.push(Line::from(vec![
                    Span::styled("  ✓ ", Style::default().fg(t.ok)),
                    Span::raw(format!("{name:<24} {d} · {ms} ms")),
                ])),
                Err(e) => lines.push(Line::from(vec![
                    Span::styled("  × ", Style::default().fg(t.danger)),
                    Span::raw(format!("{name:<24} {}", truncate(e, 60))),
                ])),
            }
        }
    }
    let _ = Screen::DataSources;
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "status", false)), status);
}

// ---------------------------------------------------------------- Trade › Markets

/// Price with sensible precision: `12,345.67`, `1.2345`, `0.009102`, `0.00000412`.
pub fn fmt_price(p: f64) -> String {
    if !p.is_finite() {
        return "—".into();
    }
    if p >= 1000.0 {
        amount::group_thousands(&format!("{p:.2}"))
    } else if p >= 1.0 {
        format!("{p:.4}")
    } else if p >= 0.001 {
        let digits = (-p.log10()).ceil() as usize + 3;
        format!("{p:.digits$}")
    } else if let Some(tiny) = amount::subscript_zeros(p, 4) {
        // Tiny prices keep one width and cannot be misread by a zero: 0.0₆1493.
        tiny
    } else {
        "0".into()
    }
}

/// Token amounts in tables: `1,764,548`, `25.61`, `0.0042`.
pub fn fmt_qty(v: f64) -> String {
    if v >= 1000.0 {
        amount::group_thousands(&format!("{v:.0}"))
    } else if v >= 1.0 {
        format!("{v:.2}")
    } else {
        fmt_price(v)
    }
}

fn pct_span(t: &Theme, pct: Option<f64>) -> Span<'static> {
    // The arrow carries the sign, so the number never needs a minus; it fits seven cells up to
    // ±999% and a flat pair reads as flat, not as a green arrow.
    match pct {
        Some(c) if c.abs() < 0.005 => Span::styled("0.00%", t.dim_style()),
        Some(c) => {
            let a = c.abs();
            let n = if a >= 100.0 {
                format!("{a:.0}")
            } else if a >= 10.0 {
                format!("{a:.1}")
            } else {
                format!("{a:.2}")
            };
            Span::styled(format!("{}{n}%", if c > 0.0 { "▲" } else { "▼" }), Style::default().fg(if c > 0.0 { t.ok } else { t.danger }))
        }
        None => Span::styled("—", t.dim_style()),
    }
}

pub fn draw_markets(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::markets::{TIMEFRAMES, Venue};
    let mv = &app.eco.markets_view;
    let (pools, overview) = match &mv.pools {
        None => {
            let block = panel(t, "markets · Quainance", true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            return empty_state(f, inner, t, spinner(), "Loading pools…", &[]);
        }
        Some(Err(e)) => {
            let block = panel(t, "markets · Quainance", true);
            let inner = block.inner(area);
            f.render_widget(block, area);
            return empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]);
        }
        Some(Ok((p, o))) => (p, o),
    };
    if pools.is_empty() {
        let block = panel(t, "markets · Quainance", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty_state(f, inner, t, "○", "No pools on this network.", &[]);
    }
    let now = wallet_core::registry::now();
    // What the filters keep. The chart, the flow and the alerts all follow this list.
    let rows_pools = app.market_rows();
    let selected = app.markets_pair().min(rows_pools.len().saturating_sub(1));
    let wide = area.width >= 110;
    // The left column carries the pairs and, under them, the DEX-wide flow; the chart keeps its
    // own width. A narrow terminal stacks them and only shows the flow when there is height for it.
    let (list_area, main, flow_area) = if wide {
        let column = if area.width >= 140 { 54 } else { 46 };
        let [left, main] = Layout::horizontal([Constraint::Length(column), Constraint::Min(60)]).areas(area);
        let flow_h = (left.height * 45 / 100).clamp(7, 22).min(left.height.saturating_sub(7));
        let [list, flow] = Layout::vertical([Constraint::Min(6), Constraint::Length(flow_h)]).areas(left);
        (list, main, (flow_h >= 5).then_some(flow))
    } else if area.height >= 28 {
        let [list, main, flow] = Layout::vertical([Constraint::Length(7), Constraint::Min(12), Constraint::Length(8)]).areas(area);
        (list, main, Some(flow))
    } else {
        let [list, main] = Layout::vertical([Constraint::Length(7), Constraint::Min(12)]).areas(area);
        (list, main, None)
    };
    if let Some(flow) = flow_area {
        draw_dex_flow(f, app, t, flow, pools);
    }

    // Pairs list with the DEX overview in its title.
    let tvl = overview.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into());
    let vol = overview.volume_24h_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into());
    // A list a ceiling shortened says so, first, so that when the panel clips its title it is the
    // TVL that goes and not the warning. It keeps the newest markets — the half worth having — but
    // a directory that is quietly partial is worse than one that admits it. The per-source detail
    // is in `quai-terminal markets` and its JSON.
    let partial = wallet_core::markets::Omitted::badge(&overview.omitted, "not listed").map(|b| format!(" · {b}")).unwrap_or_default();
    // An order other than the default says so, so a list that is not in its usual order explains
    // itself rather than looking like missing data.
    let ordered = match app.eco.markets_view.sort {
        super::eco::MarketSort::Default => String::new(),
        sort => format!(" · by {}", sort.label()),
    };
    let stale = overview.sources.is_empty() || overview.sources.iter().any(|source| !source.fresh_at(now));
    let freshness = if stale { " · stale/partial source" } else { "" };
    let reserves = app
        .eco
        .markets_view
        .reserves_at
        .map(|at| format!(" · reserves {}s", at.elapsed().as_secs()))
        .unwrap_or_else(|| " · reserves unverified".into());
    let title = if overview.source == "chain" {
        format!("pairs{partial}{freshness}{reserves}{ordered} · {} · from the node", pools.len())
    } else {
        format!("pairs{partial}{freshness}{reserves}{ordered} · TVL {tvl} · 24h {vol}")
    };
    let block = panel(t, &title, app.screen == Screen::Markets && app.pane == 0);
    let inner = block.inner(list_area);
    f.render_widget(block, list_area);
    let visible = inner.height.saturating_sub(1) as usize;
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    let rows: Vec<Row> = rows_pools
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(i, p)| {
            let base0 = app.pool_base0(p);
            let (base, quote) = if base0 { (&p.token0, &p.token1) } else { (&p.token1, &p.token0) };
            let name = format!("{}/{}", app.market_symbol(base), app.market_symbol(quote));
            let stats = matches!(mv.events.get(&p.address), Some(Ok(_))).then(|| app.market_stats(p, base0, now));
            let price = match &stats {
                Some(stats) => stats.price,
                _ => listed_price(p, base0),
            };
            // The pool's own trades when they are loaded; otherwise the indexer's day-ago price, so
            // every row has a change and not only the one that was opened.
            let listed = p.change_24h().map(|c| if base0 { c } else { (100.0 / (100.0 + c) - 1.0) * 100.0 });
            let change = match &stats {
                Some(stats) => pct_span(t, stats.change_24h.or(listed)),
                _ => pct_span(t, listed),
            };
            let icons = [base, quote].map(|tok| images::asset_span(app, t, &app.pool_icon_contract(tok), &app.market_symbol(tok)));
            let [base_icon, quote_icon] = icons;
            // Where it trades: a graduated launch is marked, a curve shows how far it has raised,
            // and a pair on the older exchange says so — its depth and its fees are its own.
            let marker = match p.venue {
                Venue::LaunchAmm => Span::styled(" ◈", Style::default().fg(t.focus)),
                Venue::Curve => Span::styled(" ○", Style::default().fg(t.attention)),
                Venue::Legacy => Span::styled(" ◌", Style::default().fg(t.attention)),
                Venue::HartiiAmm => Span::styled(" H", Style::default().fg(t.focus)),
                Venue::Main => Span::raw(""),
            };
            // Watched pairs sit at the top, marked.
            let watched = app.eco.watchlist.iter().any(|w| w.eq_ignore_ascii_case(&p.address));
            let watch = Span::styled(if watched { " ●" } else { "" }, Style::default().fg(t.attention));
            let depth = match &p.curve {
                Some(c) => Span::styled(format!("{}%", c.progress_bps.unwrap_or(0) / 100), Style::default().fg(t.attention)),
                None => Span::styled(p.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_default(), t.dim_style()),
            };
            let row = Row::new(vec![
                Cell::from(Line::from(vec![
                    base_icon,
                    quote_icon,
                    Span::raw(" "),
                    Span::styled(truncate(&name, 12), t.strong_style()),
                    marker,
                    watch,
                ])),
                Cell::from(Line::from(price.map(fmt_price).unwrap_or_else(|| "—".into())).alignment(Alignment::Right)),
                Cell::from(change),
                Cell::from(Line::from(depth).alignment(Alignment::Right)),
            ]);
            if i == selected { row.style(t.selected()) } else { row }
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Min(12), Constraint::Length(9), Constraint::Length(7), Constraint::Length(7)])
            .column_spacing(1)
            .header(Row::new(["pair", "price", "24h", "TVL/%"]).style(t.dim_style())),
        inner,
    );

    // The selected pair — from the list as it is shown, since that is what `selected` indexes.
    // Reading it out of the unordered directory instead put the chart on a different pair than the
    // cursor as soon as the list was sorted or a pair was watched.
    let pool = &rows_pools[selected];
    let base0 = app.pool_base0(pool);
    let (base, quote) = if base0 { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
    let (base_sym, quote_sym) = (app.market_symbol(base), app.market_symbol(quote));
    let (tf_label, bucket) = TIMEFRAMES[mv.timeframe];
    let events = match mv.events.get(&pool.address) {
        Some(Ok(ev)) => Some(ev.as_slice()),
        _ => None,
    };
    let loading = mv.events_loading.as_deref() == Some(pool.address.as_str());
    let action = if pool.venue == Venue::Curve { "t buy on the curve" } else { "t trade" };
    let title = format!(
        "{base_sym}/{quote_sym} · {} · {tf_label} · T timeframe · f flip · {action}{}",
        pool.venue.label(),
        if loading { " · updating…" } else { "" }
    );
    let block = panel(t, &title, false);
    let inner = block.inner(main);
    f.render_widget(block, main);
    let [header, chart_area, volume_area, tape_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(3),
        Constraint::Length(if inner.height > 26 { 9 } else { 5 }),
    ])
    .areas(inner);

    let stats = events.map(|_| app.market_stats(pool, base0, now)).unwrap_or_default();
    let quote_usd = app.token_usd(quote);
    let usd = |q: f64| quote_usd.map(|p| format!(" ({})", amount::usd(q * p))).unwrap_or_default();
    let price = stats.price.or_else(|| listed_price(pool, base0));
    let mut line1 = vec![
        images::asset_span(app, t, &app.pool_icon_contract(base), &base_sym),
        images::asset_span(app, t, &app.pool_icon_contract(quote), &quote_sym),
        Span::raw(" "),
        Span::styled(format!("{} {quote_sym}", price.map(fmt_price).unwrap_or_else(|| "—".into())), t.strong_style().fg(t.focus)),
        Span::raw("  "),
        pct_span(t, stats.change_24h),
        Span::styled("  24h", t.dim_style()),
    ];
    let covered = app.eco.markets_view.history_coverage.get(&pool.address).is_some_and(|c| {
        c.complete
            && c.cursor.is_none()
            && c.tail_cursor.is_none()
            && c.since <= now.saturating_sub(86_400)
            && c.canonical
                .as_ref()
                .is_some_and(|checked| checked.covers_recorded_positions() && now.saturating_sub(checked.checked_at) < 90)
    });
    if !covered {
        line1.push(Span::styled(" · partial history", Style::default().fg(t.attention)));
    }
    if let Some(curve) = &pool.curve {
        line1.push(Span::styled(format!(" · {}", curve.price_basis.label()), t.dim_style()));
    }
    if let (Some(h), Some(l)) = (stats.high_24h, stats.low_24h) {
        line1.push(Span::styled(format!("  H {}  L {}", fmt_price(h), fmt_price(l)), t.dim_style()));
    }
    let holdings = holding_line(app, base, quote, &base_sym, &quote_sym);
    // A curve has no pool to measure: it has what it raised toward graduation.
    let depth = match &pool.curve {
        Some(c) => format!(
            " · raised {} of {} QUAI ({}%)",
            fmt_qty(c.raised_quai),
            c.target_quai.map(fmt_qty).unwrap_or_else(|| "—".into()),
            c.progress_bps.unwrap_or(0) / 100
        ),
        None => format!(" · TVL {}", pool.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into())),
    };
    // The pair's alerts, where its price is.
    let set: Vec<String> = app
        .eco
        .alerts
        .iter()
        .filter(|a| a.pool.eq_ignore_ascii_case(&pool.address))
        .map(|a| a.describe().trim_start_matches(&format!("{} ", a.name)).to_string())
        .collect();
    if !set.is_empty() {
        line1.push(Span::styled(format!("   ● alert {}", set.join(" · ")), Style::default().fg(t.attention)));
    }
    let line2 = vec![
        Span::styled(format!("vol {} {quote_sym}{}", fmt_qty(stats.volume_24h), usd(stats.volume_24h)), t.text_style()),
        Span::styled(format!(" · {} trades", stats.trades_24h), t.dim_style()),
        Span::styled(depth, if pool.curve.is_some() { Style::default().fg(t.attention) } else { t.dim_style() }),
        Span::styled(holdings, t.dim_style()),
    ];
    f.render_widget(Paragraph::new(vec![Line::from(line1), Line::from(line2)]), header);

    match events {
        None if loading || !mv.events.contains_key(&pool.address) => {
            empty_state(f, chart_area, t, spinner(), "Reading pool history…", &[]);
        }
        None => {
            let e = match mv.events.get(&pool.address) {
                Some(Err(e)) => app::friendly_error(e),
                _ => String::new(),
            };
            empty_state(f, chart_area, t, "×", &e, &[("R", "retry")]);
        }
        Some(ev) => {
            let chart_w = chart_area.width.saturating_sub(11).max(8);
            let step = if chart_w as usize / 2 >= 24 { 2 } else { 1 };
            let n = ((chart_w / step) as usize).min(super::eco::MARKET_CANDLES);
            let _ = ev;
            let cs = app.chart_candles(pool, base0, bucket, n);
            if cs.is_empty() {
                empty_state(f, chart_area, t, "○", "No trades in this window yet.", &[("T", "longer timeframe")]);
            } else {
                draw_candles(f, t, chart_area, &cs, step, bucket);
                draw_volume(f, t, volume_area, &cs, step);
            }
            draw_trade_tape(f, app, t, tape_area, &app.market_trades(pool, base0), &base_sym, &quote_sym);
        }
    }
}

/// A market's price before its history loads, base-per-quote as the list shows it: from the pool's
/// reserves, or a curve's own mark.
fn listed_price(pool: &wallet_core::markets::Pool, base0: bool) -> Option<f64> {
    pool.spot_price().map(|p| if base0 { p } else { 1.0 / p })
}

fn holding_line(app: &App, base: &wallet_core::markets::PoolToken, quote: &wallet_core::markets::PoolToken, bs: &str, qs: &str) -> String {
    let Some(p) = &app.eco.portfolio else { return String::new() };
    let wquai = app.config.network(&app.network_id).ok().and_then(|n| n.wquai).map(|w| w.to_lowercase());
    let held = |tok: &wallet_core::markets::PoolToken| {
        p.rows
            .iter()
            .find(|r| match &r.key {
                AssetKey::Quai => wquai.as_deref() == Some(tok.address.as_str()),
                AssetKey::Token(a) => a.eq_ignore_ascii_case(&tok.address),
                _ => false,
            })
            .map(|r| amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4)))
            .unwrap_or_else(|| "0".into())
    };
    format!(" · you hold {} {bs} · {} {qs}", held(base), held(quote))
}

/// Candles with wicks and bodies at half-cell resolution, price axis on the right.
fn draw_candles(f: &mut Frame, t: &Theme, area: Rect, cs: &[wallet_core::markets::Candle], step: u16, bucket: u64) {
    let axis_w = 10u16;
    let plot = Rect { width: area.width.saturating_sub(axis_w + 1), height: area.height.saturating_sub(1), ..area };
    if plot.height < 3 || plot.width < 4 {
        return;
    }
    let hi = cs.iter().map(|c| c.high).fold(f64::MIN, f64::max);
    let lo = cs.iter().map(|c| c.low).fold(f64::MAX, f64::min);
    let pad = ((hi - lo) * 0.06).max(hi.abs() * 1e-6).max(1e-18);
    let (top, bottom) = (hi + pad, (lo - pad).max(0.0));
    let rows = f64::from(plot.height) * 2.0;
    // Half-cell index from the top for a price.
    let y = |p: f64| (((top - p) / (top - bottom)) * rows).clamp(0.0, rows - 1.0);
    let buf = f.buffer_mut();
    // Light grid at quarter heights.
    for r in [plot.height / 4, plot.height / 2, plot.height * 3 / 4] {
        for x in plot.left()..plot.right() {
            if let Some(cell) = buf.cell_mut((x, plot.y + r)) {
                cell.set_char('┈').set_style(t.dim_style());
            }
        }
    }
    for (i, c) in cs.iter().enumerate() {
        let x = plot.x + i as u16 * step;
        if x >= plot.right() {
            break;
        }
        let up = c.close >= c.open;
        let style = Style::default().fg(if up { t.ok } else { t.danger });
        // A bucket without trades carries the last price: a thin level line, not a candle.
        if c.trades == 0 && (c.high - c.low).abs() <= f64::EPSILON * c.high.abs().max(1.0) {
            let row = ((y(c.close) / 2.0) as u16).min(plot.height - 1);
            for dx in 0..step {
                if x + dx < plot.right()
                    && let Some(cell) = buf.cell_mut((x + dx, plot.y + row))
                {
                    cell.set_char('─').set_style(t.dim_style());
                }
            }
            continue;
        }
        let (wick_top, wick_bot) = (y(c.high), y(c.low));
        let (body_top, body_bot) = (y(c.open.max(c.close)), y(c.open.min(c.close)));
        for row in 0..plot.height {
            let (h0, h1) = (f64::from(row) * 2.0, f64::from(row) * 2.0 + 1.0);
            // Half cells of this row covered by the body and the wick.
            let body = |h: f64| h + 0.5 >= body_top && h - 0.5 <= body_bot;
            let wick = |h: f64| h + 0.5 >= wick_top && h - 0.5 <= wick_bot;
            let ch = match (body(h0), body(h1)) {
                (true, true) => Some('█'),
                (true, false) => Some('▀'),
                (false, true) => Some('▄'),
                _ if wick(h0) || wick(h1) => Some('│'),
                _ => None,
            };
            let ch = if (body_bot - body_top) < 0.6 && body(h0) != body(h1) && (body(h0) || body(h1)) { Some('─') } else { ch };
            if let Some(ch) = ch
                && let Some(cell) = buf.cell_mut((x, plot.y + row))
            {
                cell.set_char(ch).set_style(style);
            }
        }
    }
    // Price axis: top, middle, bottom.
    let axis_x = plot.right() + 1;
    for (row, p) in [(0u16, top), (plot.height / 2, (top + bottom) / 2.0), (plot.height - 1, bottom)] {
        let text = fmt_price(p);
        for (k, ch) in text.chars().take(axis_w as usize).enumerate() {
            if let Some(cell) = buf.cell_mut((axis_x + k as u16, plot.y + row)) {
                cell.set_char(ch).set_style(t.dim_style());
            }
        }
    }
    // Last price marker.
    if let Some(last) = cs.last() {
        let row = (y(last.close) / 2.0) as u16;
        let text = format!("◂{}", fmt_price(last.close));
        for (k, ch) in text.chars().take(axis_w as usize).enumerate() {
            if let Some(cell) = buf.cell_mut((axis_x + k as u16, plot.y + row.min(plot.height - 1))) {
                cell.set_char(ch).set_style(t.strong_style().fg(t.focus));
            }
        }
    }
    // Time axis: first, middle and last candle.
    let label = |ts: u64| local_time_label(ts, bucket);
    let axis_y = plot.bottom();
    let mid = cs.len() / 2;
    let mut next_free = plot.x;
    for (i, c) in [(0, &cs[0]), (mid, &cs[mid]), (cs.len() - 1, &cs[cs.len() - 1])] {
        let text = label(c.start);
        let x = (plot.x + i as u16 * step).min(plot.right().saturating_sub(text.len() as u16));
        // Labels never overlap (few candles put first, middle and last close together).
        if x < next_free {
            continue;
        }
        next_free = x + text.len() as u16 + 1;
        for (k, ch) in text.chars().enumerate() {
            if let Some(cell) = buf.cell_mut((x + k as u16, axis_y)) {
                cell.set_char(ch).set_style(t.dim_style());
            }
        }
    }
}

/// Axis label in system local time: `14:00` for intraday candles, `Sep 14` for daily ones.
pub fn local_time_label(ts: u64, bucket: u64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(ts as i64, 0).single() {
        Some(t) if bucket >= 86_400 => t.format("%b %d").to_string(),
        Some(t) if bucket >= 14_400 => t.format("%d %H:%M").to_string(),
        Some(t) => t.format("%H:%M").to_string(),
        None => String::new(),
    }
}

fn draw_volume(f: &mut Frame, t: &Theme, area: Rect, cs: &[wallet_core::markets::Candle], step: u16) {
    let width = area.width.saturating_sub(11);
    let max = cs.iter().map(|c| c.volume).fold(0.0, f64::max);
    if max <= 0.0 || area.height == 0 {
        f.render_widget(Paragraph::new(Span::styled("no volume in this window", t.dim_style())), area);
        return;
    }
    let levels = f64::from(area.height) * 8.0;
    let buf = f.buffer_mut();
    for (i, c) in cs.iter().enumerate() {
        let x = area.x + i as u16 * step;
        if x >= area.x + width {
            break;
        }
        let mut units = ((c.volume / max) * levels).round() as i64;
        if c.volume > 0.0 {
            units = units.max(1);
        }
        let style = Style::default().fg(if c.close >= c.open { t.ok } else { t.danger });
        for row in (0..area.height).rev() {
            let fill = units.clamp(0, 8) as usize;
            units -= 8;
            if fill == 0 {
                break;
            }
            if let Some(cell) = buf.cell_mut((x, area.y + row)) {
                cell.set_symbol(BARS[fill - 1]).set_style(style);
            }
        }
    }
    let label = format!("vol max {}", fmt_qty(max));
    let lx = area.x + width + 1;
    for (k, ch) in label.chars().take(10).enumerate() {
        if let Some(cell) = buf.cell_mut((lx + k as u16, area.y)) {
            cell.set_char(ch).set_style(t.dim_style());
        }
    }
}

/// Age in seconds-resolution, for feeds that live in seconds: `3s`, `42s`, `7m`, `2h`.
pub(crate) fn flow_age(at: u64) -> String {
    let age = wallet_core::registry::now().saturating_sub(at);
    match age {
        0..60 => format!("{age}s"),
        60..3600 => format!("{}m", age / 60),
        3600..86_400 => format!("{}h", age / 3600),
        _ => format!("{}d", age / 86_400),
    }
}

/// Every swap on the DEX as it lands, newest first: what went in, what came out, the pair's
/// price and what the trade was worth. A multi-hop route shows one row per pool it crossed,
/// the later hops marked `»`.
fn draw_dex_flow(f: &mut Frame, app: &App, t: &Theme, area: Rect, pools: &[wallet_core::markets::Pool]) {
    let mv = &app.eco.markets_view;
    let focused = app.screen == Screen::Markets && app.pane == 1;
    let floor = if mv.flow_min_usd > 0.0 { format!(" · over {}", wallet_core::swap::usd_compact(mv.flow_min_usd)) } else { String::new() };
    let title = match (&mv.flow_error, mv.flow.is_empty()) {
        (Some(_), _) => "flow · all pools · not updating".to_string(),
        (None, false) => format!("flow · all pools{floor}"),
        (None, true) => "flow · all pools".to_string(),
    };
    let block = panel(t, &title, focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if mv.flow.is_empty() {
        return match (&mv.flow_error, mv.flow_loading) {
            (Some(e), _) => empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]),
            (None, true) => empty_state(f, inner, t, spinner(), "Watching for swaps…", &[]),
            (None, false) => empty_state(f, inner, t, "○", "No swaps in the last few minutes.", &[]),
        };
    }
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let header = inner.height >= 7;
    let rows_h = inner.height.saturating_sub(u16::from(header)) as usize;
    let wide = inner.width >= 44;
    let sym_w = if inner.width >= 50 { 7 } else { 5 };
    let visible = app.flow_rows();
    if visible.is_empty() {
        let floor = wallet_core::swap::usd_compact(mv.flow_min_usd);
        return empty_state(f, inner, t, "○", &format!("No swaps over {floor} in the tape."), &[("m", "lower the floor")]);
    }
    // The cursor only lives here while this pane has the focus.
    let cursor = (focused && rows_h > 0).then(|| app.selected.min(visible.len() - 1));
    let offset = cursor.map_or(0, |c| c.saturating_sub(rows_h.saturating_sub(1)));
    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(rows_h)
        .map(|(i, s)| {
            let pool = pools.iter().find(|p| p.address == s.pool);
            let base = app.flow_base(s, pool);
            let price = base.and_then(|b| s.price(&b.address));
            let usd = app.swap_usd(s);
            let bought_base = base.is_some_and(|b| s.buys(&b.address));
            let color = match base {
                Some(_) if bought_base => t.ok,
                Some(_) => t.danger,
                None => t.text,
            };
            // The hops of one route share a transaction: only the first carries the time.
            let hop = i > 0 && visible[i - 1].tx == s.tx;
            let ours = mine.contains(&s.trader.to_lowercase());
            let when = if hop {
                Span::styled("  »", t.dim_style())
            } else if ours {
                Span::styled("you", t.strong_style().fg(t.focus))
            } else {
                Span::styled(flow_age(s.at), t.dim_style())
            };
            // Both logos, then the names once: the icon carries the symbol's letters where
            // bitmaps cannot be placed, exactly as the pairs list draws a market.
            let (from, to) = (app.market_symbol(&s.token_in), app.market_symbol(&s.token_out));
            let plain = if ours { t.strong_style().fg(t.focus) } else { t.text_style() };
            let strong = if ours { t.strong_style().fg(t.focus) } else { t.strong_style() };
            let pair = Line::from(vec![
                // Your own trades carry a mark down the column, so they are findable at a glance.
                Span::styled(if ours { "▌" } else { " " }, Style::default().fg(t.focus)),
                images::asset_span(app, t, &app.pool_icon_contract(&s.token_in), &from),
                images::asset_span(app, t, &app.pool_icon_contract(&s.token_out), &to),
                Span::raw(" "),
                Span::styled(truncate(&from, sym_w), plain),
                Span::styled("→", Style::default().fg(if ours { t.focus } else { color })),
                Span::styled(truncate(&to, sym_w), strong),
            ]);
            let mut cells = vec![
                Cell::from(Line::from(when).alignment(Alignment::Right)),
                Cell::from(pair),
                Cell::from(
                    Line::from(Span::styled(usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into()), strong))
                        .alignment(Alignment::Right),
                ),
            ];
            if wide {
                let price = price.map(fmt_price).unwrap_or_else(|| "—".into());
                cells.insert(2, Cell::from(Line::from(Span::styled(price, t.dim_style())).alignment(Alignment::Right)));
            }
            let row = Row::new(cells);
            if cursor == Some(i) { row.style(t.selected()) } else { row }
        })
        .collect();
    let mut widths = vec![Constraint::Length(3), Constraint::Min(14), Constraint::Length(8)];
    let mut titles = vec!["", "swap", "value"];
    if wide {
        widths.insert(2, Constraint::Length(9));
        titles.insert(2, "price");
    }
    let mut table = Table::new(rows, widths).column_spacing(1);
    if header {
        table = table.header(Row::new(titles).style(t.dim_style()));
    }
    f.render_widget(table, inner);
}

fn draw_trade_tape(f: &mut Frame, app: &App, t: &Theme, area: Rect, tr: &[wallet_core::markets::Trade], base: &str, quote: &str) {
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let rows: Vec<Row> = tr
        .iter()
        .take(area.height.saturating_sub(1) as usize)
        .map(|x| {
            let side =
                if x.buy { Span::styled("buy ", Style::default().fg(t.ok)) } else { Span::styled("sell", Style::default().fg(t.danger)) };
            let trader = if mine.contains(&x.trader.to_lowercase()) {
                Span::styled("you", t.strong_style().fg(t.focus))
            } else {
                Span::styled(short_address(&x.trader), t.dim_style())
            };
            Row::new(vec![
                Cell::from(Span::styled(ago(x.at), t.dim_style())),
                Cell::from(side),
                Cell::from(Line::from(fmt_price(x.price)).alignment(Alignment::Right)),
                Cell::from(Line::from(format!("{} {base}", fmt_qty(x.base))).alignment(Alignment::Right)),
                Cell::from(Line::from(format!("{} {quote}", fmt_qty(x.quote))).alignment(Alignment::Right)),
                Cell::from(trader),
            ])
        })
        .collect();
    if rows.is_empty() {
        f.render_widget(Paragraph::new(Span::styled("no trades in the loaded history", t.dim_style())), area);
        return;
    }
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Length(4),
                Constraint::Length(11),
                Constraint::Min(12),
                Constraint::Min(12),
                Constraint::Length(11),
            ],
        )
        .column_spacing(1)
        .header(Row::new(["trades", "", "price", "size", "total", "trader"]).style(t.dim_style())),
        area,
    );
}

// ---------------------------------------------------------------- People › Board

/// The on-chain message board: the channels this wallet follows, and the open one's messages
/// oldest first, the way a conversation reads. Every body was written by a stranger, so it is
/// shown only when it really is text (see `wallet_core::messages::Post::text`).
pub fn draw_board(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use super::eco::BoardRow;
    // A network without a board has nothing to read or write, and no amount of retrying will
    // change that: say what a board is and how this network comes to have one.
    if app.config.network(&app.network_id).is_ok_and(|n| n.ecosystem.messages.is_none()) {
        let block = panel(t, "board", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let lines = vec![
            Line::from(Span::styled(format!("○  No message board on {}.", app.dash.network_name), t.strong_style())),
            Line::from(""),
            Line::from(Span::styled(
                "A board is a contract with no owner. Anyone can deploy one, and this wallet reads whichever board the \
                 network is pointed at — so a board exists for everyone pointed at the same address.",
                t.dim_style(),
            )),
            Line::from(""),
            Line::from(Span::styled("Deploy one with the quai-messages project, then point this network at it:", t.dim_style())),
            Line::from(""),
            Line::from(Span::styled("    quai-terminal network add … --messages <address>", t.text_style())),
        ];
        let width = inner.width.saturating_sub(4).min(74);
        let rect = Rect::new(inner.x + 2, inner.y + inner.height.saturating_sub(9) / 2, width, inner.height.min(9));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), rect);
        return;
    }
    let rows = app.board_rows();
    let wide = area.width >= 90;
    let (list_area, main) = if wide {
        let [l, m] = Layout::horizontal([Constraint::Length(26), Constraint::Min(50)]).areas(area);
        (l, m)
    } else {
        let [l, m] = Layout::vertical([Constraint::Length(5), Constraint::Min(8)]).areas(area);
        (l, m)
    };

    // Channels first, then everyone this wallet can write to in private.
    let open = app.board_row();
    let selected = if app.pane == 1 { app.eco.board_channel_selected } else { app.selected };
    let title = match app.eco.board.filter.as_deref() {
        Some(f) => format!("filter: {f}▏"),
        None => "channels · people".to_string(),
    };
    let block = panel(t, &title, app.pane == 0);
    let inner = block.inner(list_area);
    f.render_widget(block, list_area);
    if rows.is_empty() {
        let (text, hints): (&str, &[(&str, &str)]) = match app.eco.board.filter.as_deref() {
            Some(f) if !f.trim().is_empty() => ("Nothing matches that.", &[("esc", "clear the filter")]),
            _ => ("Nothing to read yet.", &[("a", "new channel")]),
        };
        empty_state(f, inner, t, "○", text, hints);
    } else {
        // Public and private are different kinds of thing, so they are not one undifferentiated
        // list: a channel is readable by anyone who looks, a conversation by exactly two people.
        // Each group is announced, and the rows inside it carry their own mark.
        let group_of = |r: &BoardRow| match r {
            BoardRow::Channel(_) => 0u8,
            BoardRow::Peer(..) => 1,
            BoardRow::Unfollowed(..) => 2,
        };
        let heading = |g: u8| match g {
            0 => "public channels",
            1 => "sealed messages",
            _ => "not followed",
        };
        // Display lines: `None` is a heading, `Some(i)` the row at that index in `rows`. A short
        // list has no room to spend on headings, so there the marks carry the distinction alone.
        let grouped = wide;
        let mut lines: Vec<(Option<usize>, u8)> = Vec::with_capacity(rows.len() + 3);
        let mut last = None;
        for (i, r) in rows.iter().enumerate() {
            let g = group_of(r);
            if grouped && last != Some(g) {
                lines.push((None, g));
                last = Some(g);
            }
            lines.push((Some(i), g));
        }
        // Headings take room, so the list scrolls to keep the selected row on screen.
        let selected = selected.min(rows.len() - 1);
        let height = inner.height as usize;
        let at = lines.iter().position(|(i, _)| *i == Some(selected)).unwrap_or(0);
        let offset = if at >= height { at + 1 - height } else { 0 };
        let table: Vec<Row> = lines
            .iter()
            .skip(offset)
            .take(height)
            .map(|(index, group)| {
                let Some(i) = index else {
                    return Row::new(vec![
                        Cell::from(Span::styled(if *group == 1 { "◉" } else { "#" }, t.dim_style())),
                        Cell::from(Span::styled(truncate(heading(*group), 15), t.dim_style())),
                        Cell::from(""),
                    ]);
                };
                let (i, r) = (*i, &rows[*i]);
                let (label, count, sealed) = match r {
                    BoardRow::Channel(name) => {
                        // Unread wins the column: how many are waiting matters more than how
                        // many there are.
                        let unread = app.board_unread(name);
                        let n = if unread > 0 {
                            format!("{unread} new")
                        } else {
                            match app.eco.board.posts.get(name) {
                                Some(Ok(p)) => p.len().to_string(),
                                Some(Err(_)) => "×".into(),
                                None => String::new(),
                            }
                        };
                        (format!("#{}", truncate(name, 15)), n, false)
                    }
                    // Dimmed and counted: somewhere to look, not somewhere you keep.
                    BoardRow::Unfollowed(name, messages) => (format!("#{}", truncate(name, 15)), messages.to_string(), false),
                    BoardRow::Peer(code, name) => {
                        let n = match app.eco.board.dms.get(code) {
                            Some(Ok(l)) => l.len().to_string(),
                            Some(Err(_)) => "×".into(),
                            None => String::new(),
                        };
                        (truncate(name.as_deref().unwrap_or(&wallet_core::session::short_code(code)), 15), n, true)
                    }
                };
                let followed = !matches!(r, BoardRow::Unfollowed(..));
                // Pinned beside every screen, and notifying: said after the name.
                let (target, _) = App::chat_target(r);
                let pinned = app.eco.board.pin.as_deref() == Some(target.as_str());
                let notifying = app.eco.board.subs.contains(&target);
                let label = format!("{label}{}{}", if notifying { " ●" } else { "" }, if pinned { " ▸" } else { "" });
                let waiting = matches!(r, BoardRow::Channel(name) if app.board_unread(name) > 0);
                let row = Row::new(vec![
                    // A filled circle marks the rows nobody else can read.
                    Cell::from(Span::styled(if sealed { "◉" } else { " " }, Style::default().fg(t.ok))),
                    Cell::from(Span::styled(label, if followed { t.strong_style() } else { t.dim_style() })),
                    Cell::from(
                        Line::from(Span::styled(count, if waiting { t.strong_style().fg(t.focus) } else { t.dim_style() }))
                            .alignment(Alignment::Right),
                    ),
                ]);
                if i == selected { row.style(t.selected()) } else { row }
            })
            .collect();
        f.render_widget(Table::new(table, [Constraint::Length(1), Constraint::Min(8), Constraint::Length(4)]).column_spacing(1), inner);
    }

    let Some(open) = open else {
        let block = panel(t, "messages", app.pane == 1);
        let inner = block.inner(main);
        f.render_widget(block, main);
        return empty_state(f, inner, t, "○", "Follow a channel to read it.", &[("a", "follow a channel")]);
    };
    match open {
        BoardRow::Channel(channel) | BoardRow::Unfollowed(channel, _) => draw_board_channel(f, app, t, main, &channel),
        BoardRow::Peer(code, name) => draw_board_conversation(f, app, t, main, &code, name.as_deref()),
    }
}

/// The pinned chat, docked beside whatever screen is open: the latest messages, newest at the
/// bottom, and the key that writes to it.
pub fn draw_chat_dock(f: &mut Frame, app: &App, t: &Theme, area: Rect, pin: &str) {
    let label = app.chat_label(pin);
    let notifying = app.eco.board.subs.iter().any(|s| s == pin);
    let title = format!("{label}{}", if notifying { " · ●" } else { "" });
    let block = panel(t, &title, app.dock_focus);
    let inner = block.inner(area);
    f.render_widget(block, area);
    // The message box on the last row: what is being written, or how to start.
    let [inner, composer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let room = composer.width.saturating_sub(3) as usize;
    let draft = &app.dock_draft;
    // The end of a long draft, where the cursor is.
    let shown: String = {
        let chars: Vec<char> = draft.chars().collect();
        chars[chars.len().saturating_sub(room.saturating_sub(1))..].iter().collect()
    };
    let composer_line = if app.dock_focus {
        Line::from(vec![Span::styled("› ", t.strong_style().fg(t.focus)), Span::styled(format!("{shown}▏"), t.strong_style())])
    } else if !draft.is_empty() {
        Line::from(vec![Span::styled("› ", t.dim_style()), Span::styled(shown, t.dim_style())])
    } else {
        Line::from(Span::styled("› tab or ` to write", t.dim_style()))
    };
    f.render_widget(Paragraph::new(composer_line).style(Style::default().bg(t.raised)), composer);
    // (time, mine, who, text), oldest first.
    let lines: Vec<(u64, bool, String, String)> = match pin.strip_prefix("dm:") {
        Some(code) => {
            if app.locked {
                return empty_state(f, inner, t, "◌", "Unlock to read.", &[]);
            }
            match app.eco.board.dms.get(code) {
                Some(Ok(l)) => l
                    .iter()
                    .map(|l| {
                        let who = app.contact_name_for(&l.from).unwrap_or_else(|| short_address(&l.from));
                        (l.at, l.mine, who, l.text.clone().unwrap_or_else(|| "<cannot read>".into()))
                    })
                    .collect(),
                Some(Err(e)) => return empty_state(f, inner, t, "×", &app::friendly_error(e), &[]),
                None => return empty_state(f, inner, t, spinner(), "Opening…", &[]),
            }
        }
        None => {
            let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
            match app.eco.board.posts.get(pin.trim_start_matches('#')) {
                Some(Ok(posts)) => posts
                    .iter()
                    .rev()
                    .map(|p| {
                        let who = app.contact_name_for(&p.from).unwrap_or_else(|| short_address(&p.from));
                        (p.at, mine.contains(&p.from.to_lowercase()), who, p.text().unwrap_or_else(|| "<sealed>".into()))
                    })
                    .collect(),
                Some(Err(e)) => return empty_state(f, inner, t, "×", &app::friendly_error(e), &[]),
                None => return empty_state(f, inner, t, spinner(), "Reading…", &[]),
            }
        }
    };
    if lines.is_empty() {
        return empty_state(f, inner, t, "○", "Quiet so far.", &[]);
    }
    // Every message whole: the first line after its age and sender, the rest wrapped beneath,
    // indented under the sender. Filled from the newest up, so the latest is always in view.
    let width = inner.width as usize;
    let height = inner.height as usize;
    let mut rows: Vec<Line> = Vec::new();
    for (at, mine, who, text) in lines.iter().rev() {
        let who = if *mine { "you".to_string() } else { truncate(who, 12) };
        let lead = 5 + unicode_width::UnicodeWidthStr::width(who.as_str()) + 2;
        let body_style = if *mine { t.strong_style() } else { t.text_style() };
        let wrapped = wrap_words(text, width.saturating_sub(lead).max(4), width.saturating_sub(CONTINUE).max(4));
        let mut message: Vec<Line> = Vec::with_capacity(wrapped.len());
        for (i, part) in wrapped.into_iter().enumerate() {
            message.push(if i == 0 {
                Line::from(vec![
                    Span::styled(format!("{:>4} ", ago_short(wallet_core::registry::now().saturating_sub(*at))), t.dim_style()),
                    Span::styled(who.clone(), if *mine { t.strong_style().fg(t.focus) } else { t.strong_style().fg(t.link) }),
                    Span::styled("  ", t.dim_style()),
                    Span::styled(part, body_style),
                ])
            } else {
                Line::from(vec![Span::raw(" ".repeat(CONTINUE)), Span::styled(part, body_style)])
            });
        }
        // Newest last: this message goes above what is already stacked.
        message.append(&mut rows);
        rows = message;
        if rows.len() >= height {
            break;
        }
    }
    // A message taller than what is left shows its end, where the newest words are; a quiet
    // chat sits at the bottom, as a conversation does.
    let skip = rows.len().saturating_sub(height);
    let mut shown = rows.split_off(skip);
    let mut padded = vec![Line::from(""); height.saturating_sub(shown.len())];
    padded.append(&mut shown);
    f.render_widget(Paragraph::new(padded), inner);
}

/// Where a dock message's wrapped lines start: under the sender, past the age.
const CONTINUE: usize = 5;

/// Words into lines of at most `first` columns, then `rest`. A word longer than a line (an
/// address, a link) is split across lines rather than cut.
pub(crate) fn wrap_words(text: &str, first: usize, rest: usize) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let mut out: Vec<String> = Vec::new();
    let mut line = String::new();
    let mut used = 0usize;
    let limit = |n: usize| if n == 0 { first } else { rest };
    for word in text.split_whitespace() {
        let w = unicode_width::UnicodeWidthStr::width(word);
        let room = limit(out.len());
        let gap = usize::from(!line.is_empty());
        if used + gap + w <= room {
            if gap == 1 {
                line.push(' ');
            }
            line.push_str(word);
            used += gap + w;
            continue;
        }
        if !line.is_empty() && w <= limit(out.len() + 1) {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
            used = w;
            continue;
        }
        // Too long for any line: fill this one, then carry on across the next.
        if !line.is_empty() {
            if used + 1 < limit(out.len()) {
                line.push(' ');
                used += 1;
            } else {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
        }
        for c in word.chars() {
            let cw = c.width().unwrap_or(0);
            if used > 0 && used + cw > limit(out.len()) {
                out.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push(c);
            used += cw;
        }
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

/// One public channel, oldest first.
fn draw_board_channel(f: &mut Frame, app: &App, t: &Theme, area: Rect, channel: &str) {
    let loading = app.eco.board.loading.as_deref() == Some(channel);
    let title = format!("#{channel}{}", if loading { " · reading…" } else { "" });
    let block = panel(t, &title, app.pane == 1);
    let inner = block.inner(area);
    f.render_widget(block, area);
    match app.eco.board.posts.get(channel) {
        Some(Err(e)) => return empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]),
        None if loading => return empty_state(f, inner, t, spinner(), "Reading the board…", &[]),
        None => return empty_state(f, inner, t, "○", "Nothing read yet.", &[("R", "read")]),
        Some(Ok(_)) => {}
    }
    let posts = app.board_posts();
    if posts.is_empty() {
        return empty_state(f, inner, t, "○", "No messages in this channel yet.", &[("p", "post the first")]);
    }
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let lines: Vec<(u64, bool, String, Option<String>)> = posts
        .iter()
        .map(|p| {
            let body = match p.text() {
                Some(text) => Some(text),
                // A sealed or unreadable body is described, never guessed at.
                None if p.kind == wallet_core::messages::KIND_TEXT => None,
                None => Some(format!("<sealed, {} bytes>", p.body.len())),
            };
            // A post from someone in your contacts reads as their name, not their address.
            let who = app.contact_name_for(&p.from).unwrap_or_else(|| short_address(&p.from));
            (p.at, mine.contains(&p.from.to_lowercase()), who, body)
        })
        .collect();
    draw_message_rows(f, app, t, inner, &lines);
}

/// One sealed conversation. Reading it needs the wallet unlocked, because the key is derived
/// from its payment account.
fn draw_board_conversation(f: &mut Frame, app: &App, t: &Theme, area: Rect, code: &str, name: Option<&str>) {
    // The code is in the title even when the peer has a name: a conversation is with a payment
    // code, not with a contact, and a name over the wrong code is exactly how two people end up
    // in two different conversations, each seeing only what they sent.
    let short = wallet_core::session::short_code(code);
    let who = match name {
        Some(name) => format!("{name} · {short}"),
        None => short,
    };
    let loading = app.eco.board.dm_loading.as_deref() == Some(code);
    let title = format!("{who} · sealed{}", if loading { " · reading…" } else { "" });
    let block = panel(t, &title, app.pane == 1);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.locked {
        return empty_state(f, inner, t, "◌", "Unlock to read this conversation.", &[]);
    }
    match app.eco.board.dms.get(code) {
        Some(Err(e)) => return empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]),
        None if loading => return empty_state(f, inner, t, spinner(), "Opening the conversation…", &[]),
        None => return empty_state(f, inner, t, "○", "Nothing read yet.", &[("R", "read")]),
        Some(Ok(_)) => {}
    }
    let lines = app.board_dm_lines();
    if lines.is_empty() {
        return empty_state(f, inner, t, "○", "Nothing between you yet.", &[("p", "write the first"), ("c", "name them")]);
    }
    let rows: Vec<(u64, bool, String, Option<String>)> = lines
        .iter()
        .map(|l| {
            let who = app.contact_name_for(&l.from).unwrap_or_else(|| short_address(&l.from));
            // Posted from an account not on record for this contact. The body opening does not prove
            // who posted it, so it is pointed out; `c` records the account if the user knows it.
            let text = match (&l.text, l.new_address) {
                (Some(text), true) => Some(format!("{text}   · from an unrecorded address {}", short_address(&l.from))),
                _ => l.text.clone(),
            };
            (l.at, l.mine, who, text)
        })
        .collect();
    draw_message_rows(f, app, t, inner, &rows);
}

/// Messages as a conversation: newest at the bottom, your own marked, and a body that would not
/// open described rather than guessed at.
fn draw_message_rows(f: &mut Frame, app: &App, t: &Theme, area: Rect, lines: &[(u64, bool, String, Option<String>)]) {
    let height = area.height as usize;
    let cursor = (app.pane == 1).then(|| app.selected.min(lines.len().saturating_sub(1)));
    let offset = cursor.map_or(lines.len().saturating_sub(height), |c| if c >= height { c + 1 - height } else { 0 });
    let rows: Vec<Row> = lines
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, (at, mine, from, body))| {
            let who = if *mine { Span::styled("you", t.strong_style().fg(t.focus)) } else { Span::styled(from.clone(), t.dim_style()) };
            let text = match body {
                Some(text) => Span::styled(text.clone(), if *mine { t.strong_style() } else { t.text_style() }),
                None => Span::styled("<cannot read this>", t.dim_style()),
            };
            let row = Row::new(vec![
                Cell::from(Line::from(Span::styled(flow_age(*at), t.dim_style())).alignment(Alignment::Right)),
                Cell::from(Line::from(who).alignment(Alignment::Right)),
                Cell::from(Line::from(text)),
            ]);
            if cursor == Some(i) { row.style(t.selected()) } else { row }
        })
        .collect();
    f.render_widget(Table::new(rows, [Constraint::Length(5), Constraint::Length(12), Constraint::Min(20)]).column_spacing(1), area);
}

// ---------------------------------------------------------------- System › Wallets

/// The wallets on this computer. Each is a separate vault with its own keys, addresses and
/// history, and only the open one is unlocked — switching drops the keys of the one leaving.
pub fn draw_wallets(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::registry::WalletKind;
    use wallet_core::sdk::U256;
    // QUAI's price, to carry a wallet's summary forward to its live QUAI balance.
    let quai_usd = app.eco.portfolio.as_ref().and_then(|p| p.rows.iter().find(|r| r.key == AssetKey::Quai)).and_then(|r| r.price_usd);
    // Value: the last priced total, moved by however much QUAI has changed since.
    let value = |id: &str| -> Option<f64> {
        let s = app.wallet_summaries.get(id)?;
        let then = amount::to_f64(s.quai.parse::<U256>().unwrap_or_default(), 18);
        let now = app.wallet_quai.get(id).map(|q| amount::to_f64(*q, 18));
        Some(match (now, quai_usd) {
            (Some(n), Some(price)) => (s.total_usd + (n - then) * price).max(0.0),
            _ => s.total_usd,
        })
    };
    let total: f64 = app.wallets.iter().filter_map(|w| value(&w.id)).sum();
    let title = if app.wallets.len() > 1 && total > 0.0 {
        format!("wallets on this computer · {} · {} together", app.wallets.len(), amount::usd(total))
    } else {
        "wallets on this computer".to_string()
    };
    let block = panel(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.wallets.is_empty() {
        return empty_state(f, inner, t, "○", "No wallets yet.", &[("a", "create one"), ("i", "import a phrase")]);
    }
    let open = app.meta.as_ref().map(|m| m.id.clone());
    let wide = inner.width >= 120;
    let now = wallet_core::registry::now();
    let rows: Vec<Row> = app
        .wallets
        .iter()
        .enumerate()
        .take(inner.height.saturating_sub(1) as usize)
        .map(|(i, w)| {
            let current = open.as_deref() == Some(w.id.as_str());
            let kind = match w.kind {
                WalletKind::Hd => "recovery phrase",
                WalletKind::Keys => "imported keys",
                WalletKind::Watch => "watch-only",
            };
            // A phrase nobody has written down is the one thing worth nagging about here.
            let note = match (w.kind, w.backed_up) {
                (WalletKind::Hd, false) => Span::styled("phrase not verified", Style::default().fg(t.attention)),
                _ => Span::styled("", t.dim_style()),
            };
            let kind = if wide { kind } else { kind.split(' ').next_back().unwrap_or(kind) };
            let summary = app.wallet_summaries.get(&w.id);
            let live = app.wallet_quai.get(&w.id);
            let quai = match (live, summary) {
                (Some(q), _) => Span::styled(amount::group_thousands(&amount::format_amount_short(*q, 18, 2)), t.text_style()),
                (None, Some(s)) => Span::styled(
                    amount::group_thousands(&amount::format_amount_short(s.quai.parse().unwrap_or_default(), 18, 2)),
                    t.dim_style(),
                ),
                _ => Span::styled("—", t.dim_style()),
            };
            let qi = summary
                .map(|s| amount::qi(s.qi.parse().unwrap_or_default()))
                .map_or_else(|| Span::styled("—", t.dim_style()), |q| Span::styled(q, Style::default().fg(t.qi)));
            let worth = value(&w.id).map_or_else(|| Span::styled("—", t.dim_style()), |v| Span::styled(amount::usd(v), t.strong_style()));
            let seen = summary.map(|s| ago_short(now.saturating_sub(s.at))).unwrap_or_default();
            let mut cells = vec![
                Cell::from(Span::styled(if current { "▸" } else { " " }, Style::default().fg(t.focus))),
                Cell::from(Span::styled(truncate(&w.name, 22), if current { t.strong_style().fg(t.focus) } else { t.strong_style() })),
                Cell::from(Span::styled(kind, t.dim_style())),
                Cell::from(Line::from(worth).alignment(Alignment::Right)),
                Cell::from(Line::from(quai).alignment(Alignment::Right)),
                Cell::from(Line::from(qi).alignment(Alignment::Right)),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(summary.map(|s| s.top.join(" · ")).unwrap_or_default(), t.dim_style())));
                cells.push(Cell::from(Line::from(Span::styled(seen, t.dim_style())).alignment(Alignment::Right)));
            }
            cells.push(Cell::from(note));
            let row = Row::new(cells);
            if i == app.selected.min(app.wallets.len() - 1) { row.style(t.selected()) } else { row }
        })
        .collect();
    let mut widths = vec![
        Constraint::Length(1),
        Constraint::Length(if wide { 22 } else { 16 }),
        Constraint::Length(if wide { 16 } else { 12 }),
        Constraint::Length(12),
        Constraint::Length(14),
        Constraint::Length(10),
    ];
    let mut header = vec!["", "wallet", "keys", "value", "QUAI", "Qi"];
    if wide {
        widths.extend([Constraint::Length(22), Constraint::Length(6)]);
        header.extend(["holds", "priced"]);
    }
    widths.push(Constraint::Fill(1));
    header.push("");
    f.render_widget(Table::new(rows, widths).column_spacing(1).header(Row::new(header).style(t.dim_style())), inner);
}

// ---------------------------------------------------------------- Trade › Pools

/// What you provide on the left, every pool on the right — because a new position starts from the
/// directory, not from something you already hold.
pub fn draw_pools(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::gauge::apr_text;
    let title = "pools · Quainance";
    if app.config.network(&app.network_id).is_ok_and(|n| n.ecosystem.quainance_router.is_none()) {
        let block = panel(t, title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty_state(f, inner, t, "◈", "No Quainance pools on this network (mainnet only).", &[("[", "swap")]);
    }
    if let Some(Err(e)) = &app.eco.pools_view.positions {
        let block = panel(t, title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty_state(f, inner, t, "×", &app::friendly_error(e), &[("R", "retry")]);
    }
    // A deposit being composed takes the screen: it is the one thing being answered.
    if app.eco.pools_view.add.is_some() {
        return draw_add_liquidity(f, app, t, area);
    }
    let positions = app.position_rows();
    let directory = app.directory_rows();
    let now = wallet_core::registry::now();
    // Side by side when there is width for it; stacked otherwise, positions first.
    let stacked = area.width < 116;
    let [left, right] = if stacked {
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(52), Constraint::Percentage(48)]).areas(area)
    };
    draw_positions_pane(f, app, t, left, positions, now);
    draw_directory_pane(f, app, t, right, &directory, now, apr_text);
}

/// Composing a deposit, shaped like the swap card: both sides on screen, the one you type and
/// the one the pool answers with, so nothing is signed before the whole trade is visible.
fn draw_add_liquidity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use super::images::asset_span;
    let Some(card) = app.eco.pools_view.add.as_ref() else { return };
    let stacked = area.width < 100;
    let [left, right] = if stacked {
        Layout::vertical([Constraint::Length(14), Constraint::Min(6)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area)
    };
    let quote = card.quote.as_ref().filter(|_| card.quote_key == card.requested_key);
    let ok_quote = quote.and_then(|q| q.as_ref().ok());
    // One row per token: the typed side shows what was typed, the other what the pool derived.
    let side_row = |field: usize, token: &wallet_core::markets::PoolToken| {
        let typed = card.side1 == (field == 2);
        let mut value = vec![asset_span(app, t, &app.pool_icon_contract(token), &token.symbol), Span::raw(" ")];
        if typed {
            value.push(amount_span(t, &card.amount, card.field == field));
        } else if let Some(paired) = card.paired_text() {
            value.push(Span::styled(format!("≈ {paired}"), t.text_style()));
        } else if !card.amount.is_empty() {
            value.push(Span::styled(format!("{} pricing…", spinner()), t.dim_style()));
        } else {
            value.push(Span::styled("—", t.dim_style()));
        }
        let held = app.pool_token_balance(token);
        value.push(Span::styled(held.map(|h| format!("   have {h}")).unwrap_or_default(), t.dim_style()));
        card_row(t, card.field == field, &token.symbol.to_lowercase(), value)
    };
    let account = card
        .account
        .as_ref()
        .and_then(|a| app.dash.accounts.iter().find(|x| &x.address == a))
        .map(|a| format!("{} ‹›", a.label))
        .unwrap_or_else(|| "—".into());
    let mut lines = vec![
        card_row(t, card.field == 0, "account", vec![Span::styled(account, t.text_style())]),
        Line::from(""),
        Line::from(Span::styled("you deposit", t.dim_style())),
        side_row(1, &card.token0),
        side_row(2, &card.token1),
        Line::from(Span::styled(format!("        type either side · m max · the pool sets the {}", card.paired().symbol), t.dim_style())),
        Line::from(""),
        card_row(
            t,
            card.field == 3,
            "slippage",
            vec![Span::styled(format!("{:.2}%  (type to change)", f64::from(card.slippage_bps) / 100.0), t.text_style())],
        ),
    ];
    if let Some(q) = ok_quote {
        // Only the steps actually left: a side already approved for enough is not signed again.
        let mut steps: Vec<String> = Vec::new();
        if q.approval0_needed {
            steps.push(format!("approve {}", q.token0.symbol));
        }
        if q.approval1_needed {
            steps.push(format!("approve {}", q.token1.symbol));
        }
        steps.push("deposit".into());
        lines.push(Line::from(""));
        lines.push(step_line(t, &steps.iter().map(String::as_str).collect::<Vec<_>>(), 0));
        lines.push(Line::from(Span::styled("enter · review the deposit", t.strong_style().fg(t.focus))));
    }
    f.render_widget(Paragraph::new(lines).block(panel(t, &format!("add liquidity · {}", card.name), true)), left);

    let block = panel(t, "deposit", false);
    let inner = block.inner(right);
    f.render_widget(block, right);
    let mut q_lines = Vec::new();
    match quote {
        Some(Ok(q)) => {
            q_lines.push(kv(t, "you deposit", Span::styled(q.deposit_text(), t.strong_style())));
            q_lines.push(kv(t, "pool share", Span::raw(q.share_text())));
            q_lines.push(kv(
                t,
                "minimums",
                Span::raw(format!(
                    "{} {} · {} {}",
                    amount::format_amount_short(q.amount0_min, q.token0.decimals, 4),
                    q.token0.symbol,
                    amount::format_amount_short(q.amount1_min, q.token1.decimals, 4),
                    q.token1.symbol
                )),
            ));
            // What is already in place, so nobody pays a fee to approve something twice.
            let approval = |needed: bool, token: &wallet_core::markets::PoolToken| {
                if needed {
                    Span::styled(format!("{} needed", token.symbol), Style::default().fg(t.attention))
                } else {
                    Span::styled(format!("{} ✓ approved", token.symbol), Style::default().fg(t.ok))
                }
            };
            if q.allowance0.is_some() {
                q_lines.push(Line::from(vec![
                    Span::styled(format!("{:<14}", "approvals"), t.dim_style()),
                    approval(q.approval0_needed, &q.token0),
                    Span::styled("  ·  ", t.dim_style()),
                    approval(q.approval1_needed, &q.token1),
                ]));
            }
            for w in &q.warnings {
                q_lines.push(Line::from(Span::styled(format!("! {w}"), Style::default().fg(t.attention))));
            }
        }
        Some(Err(e)) => q_lines.push(Line::from(Span::styled(format!("× {}", app::friendly_error(e)), Style::default().fg(t.danger)))),
        None => {
            q_lines.push(Line::from(Span::styled("Type an amount on either side; the other follows the pool.", t.dim_style())));
            q_lines.push(Line::from(""));
            q_lines.push(Line::from(Span::styled(
                "A deposit goes in at the pool's own ratio. Typing the side you have least of is how you find the largest deposit you can actually make.",
                t.dim_style(),
            )));
        }
    }
    q_lines.push(Line::from(""));
    q_lines.push(Line::from(vec![
        key(t, "tab"),
        Span::styled("fields  ", t.dim_style()),
        key(t, "m"),
        Span::styled("max  ", t.dim_style()),
        key(t, "enter"),
        Span::styled("review  ", t.dim_style()),
        key(t, "esc"),
        Span::styled("cancel", t.dim_style()),
    ]));
    f.render_widget(Paragraph::new(q_lines).wrap(Wrap { trim: true }), inner);
}

/// Pane 0: what this wallet provides.
fn draw_positions_pane(f: &mut Frame, app: &App, t: &Theme, area: Rect, positions: &[wallet_core::liquidity::LpPosition], now: u64) {
    use wallet_core::gauge::apr_text;
    let total: f64 = positions.iter().filter_map(|p| p.usd).sum();
    let heading = if positions.is_empty() { "your liquidity".to_string() } else { format!("your liquidity · {}", amount::usd(total)) };
    let block = panel(t, &heading, app.pane == 0);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.pools_view.positions.is_none() {
        return empty_state(f, inner, t, spinner(), "Reading your liquidity…", &[]);
    }
    if positions.is_empty() {
        return empty_state(
            f,
            inner,
            t,
            "◈",
            "You provide no liquidity yet. Pick a pool on the right and press a — you earn a share of every swap fee, and some pools pay gauge rewards on top.",
            &[("tab", "pool list"), ("a", "add liquidity")],
        );
    }
    let selected = app.eco.pools_view.selected.min(positions.len() - 1);
    let mut lines = Vec::new();
    for (i, p) in positions.iter().enumerate() {
        let gauge = app.gauge_pool_for(&p.pair);
        let zone = if gauge.is_none() { app.zone_pool_for(&p.pair) } else { None };
        let apr = app.pool_apr(p);
        let pending = gauge.is_some_and(wallet_core::gauge::GaugePool::has_rewards) || zone.is_some_and(|z| z.has_rewards());
        let focused = i == selected && app.pane == 0;
        let style = if focused { t.selected() } else { t.text_style() };
        let mark = match (gauge.is_some() || zone.is_some(), !p.lp_staked.is_zero()) {
            (true, true) => Span::styled("◆ staked", Style::default().fg(t.ok)),
            (true, false) => Span::styled("○ stakeable", Style::default().fg(t.attention)),
            (false, _) => Span::raw(""),
        };
        let mut row = vec![Span::styled(if focused { "▌" } else { " " }, Style::default().fg(t.focus))];
        row.extend(pair_icons(app, t, &p.token0, &p.token1));
        row.extend([
            Span::styled(format!("{:<14}", truncate(&p.name(), 14)), style.add_modifier(Modifier::BOLD)),
            Span::styled(format!("{:>8}  ", p.share_text()), t.dim_style()),
            Span::styled(format!("{:>10}", p.usd.map(amount::usd).unwrap_or_else(|| "—".into())), style),
            Span::styled(
                format!("  {:>6}", if gauge.is_some() || zone.is_some() { apr_text(apr) } else { String::new() }),
                Style::default().fg(t.ok),
            ),
        ]);
        lines.push(Line::from(row));
        lines.push(Line::from(vec![
            Span::styled(format!("    └ {}", p.underlying_text()), t.dim_style()),
            Span::raw(" "),
            mark,
            Span::styled(if pending { "  ● rewards" } else { "" }, Style::default().fg(t.attention)),
        ]));
        // The focused position spells out its rewards rather than making the user open a detail.
        if focused && let Some(g) = gauge {
            for reward in &g.rewards {
                lines.push(Line::from(Span::styled(
                    format!(
                        "      {} claimable · {} · {:.2}/day to the pool",
                        reward.earned_text(),
                        reward.period_text(now),
                        reward.per_day()
                    ),
                    Style::default().fg(if reward.earned.is_zero() { t.dim } else { t.ok }),
                )));
            }
            if p.lp_staked.is_zero() && !p.lp_wallet.is_zero() && g.active(now) {
                lines.push(Line::from(Span::styled("      press s to stake and start earning", Style::default().fg(t.attention))));
            }
        }
        // A launch-zone campaign on this pair, which the core gauge knows nothing about.
        if focused && let Some(z) = zone {
            lines.extend(zone_campaign_lines(app, t, z, now, app.pool_tvl(&p.pair)));
            if p.lp_staked.is_zero() && !p.lp_wallet.is_zero() {
                lines.push(Line::from(Span::styled("        press s to stake in the launch-zone gauge", Style::default().fg(t.attention))));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        key(t, "a"),
        Span::styled("add  ", t.dim_style()),
        key(t, "r"),
        Span::styled("remove  ", t.dim_style()),
        key(t, "s"),
        Span::styled("stake  ", t.dim_style()),
        key(t, "u"),
        Span::styled("unstake  ", t.dim_style()),
        key(t, "h"),
        Span::styled("harvest  ", t.dim_style()),
        key(t, "i"),
        Span::styled("fund", t.dim_style()),
    ]));
    f.render_widget(Paragraph::new(lines), inner);
}

/// What a launch-zone campaign is paying, and where it stands. Its rewards are streamed by a
/// different gauge from the core one, so the lines say which is which rather than adding them up.
fn zone_campaign_lines(app: &App, t: &Theme, z: &wallet_core::zone::ZonePool, now: u64, tvl_usd: Option<f64>) -> Vec<Line<'static>> {
    use wallet_core::gauge::apr_text;
    use wallet_core::zone::Genesis;
    let state = z.campaign.state(now);
    let colour = match state {
        Genesis::Live => t.ok,
        Genesis::AwaitingActivation => t.attention,
        _ => t.dim,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled("      ◈ launch-zone campaign · ", Style::default().fg(t.attention)),
        Span::styled(state.text(), Style::default().fg(colour)),
        Span::styled(
            match state {
                Genesis::Live => format!("  ·  {} APR", apr_text(app.zone_apr(z, tvl_usd))),
                _ => String::new(),
            },
            Style::default().fg(t.ok),
        ),
    ])];
    if let Some(text) = z.reward_text(now) {
        lines.push(Line::from(Span::styled(format!("        {text}"), t.dim_style())));
    }
    if !z.staked.is_zero() {
        lines.push(Line::from(Span::styled(
            format!("        {} LP staked here", amount::format_amount_short(z.staked, 18, 6)),
            t.dim_style(),
        )));
    }
    for reward in z.rewards.iter().filter(|r| !r.earned.is_zero()) {
        lines.push(Line::from(Span::styled(format!("        {} claimable · h claims", reward.earned_text()), Style::default().fg(t.ok))));
    }
    if state == Genesis::AwaitingActivation {
        lines.push(Line::from(Span::styled(
            format!("        starts once {:.0}% of the activation stake is staked", z.activation_bps() as f64 / 100.0),
            Style::default().fg(t.attention),
        )));
    }
    lines
}

/// Pane 1: every pool, so a position can be opened in one.
fn draw_directory_pane(
    f: &mut Frame,
    app: &App,
    t: &Theme,
    area: Rect,
    pools: &[wallet_core::markets::Pool],
    now: u64,
    apr_text: fn(Option<f64>) -> String,
) {
    // The gauges are read with a ceiling too, and an APR column that is silently missing rows is
    // the same lie as a market list that is. Short and first, for the same reason as Markets.
    let omitted: Vec<wallet_core::markets::Omitted> = app
        .eco
        .pools_view
        .gauge
        .as_ref()
        .and_then(|g| g.omitted.clone())
        .into_iter()
        .chain(app.eco.pools_view.zone.as_ref().into_iter().flat_map(|z| z.omitted.iter().cloned()))
        .collect();
    let title = match wallet_core::markets::Omitted::badge(&omitted, "gauge pools unread") {
        None => "all pools · tab to switch · a to add".to_string(),
        Some(badge) => format!("all pools · {badge} · a to add"),
    };
    let block = panel(t, &title, app.pane == 1);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if pools.is_empty() {
        return empty_state(f, inner, t, spinner(), "Loading pools…", &[]);
    }
    let selected = app.eco.pools_view.pool_selected.min(pools.len() - 1);
    let height = inner.height.saturating_sub(1) as usize;
    let start = selected.saturating_sub(height.saturating_sub(2));
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:<6}{:<16}", "", "pair"), t.dim_style()),
        Span::styled(format!("{:>9}  ", "TVL"), t.dim_style()),
        Span::styled(format!("{:>6}", "apr"), t.dim_style()),
    ])];
    for (i, pool) in pools.iter().enumerate().skip(start).take(height) {
        let focused = i == selected && app.pane == 1;
        let style = if focused { t.selected() } else { t.text_style() };
        let name = format!("{}/{}", pool.token0.symbol, pool.token1.symbol);
        let held = app.position_rows().iter().any(|p| p.pair.eq_ignore_ascii_case(&pool.address));
        // The gauge's APR is a property of the pool, so it belongs here too — it is the main
        // reason to choose one pool over another.
        let gauge = app.gauge_pool_for(&pool.address);
        let apr = gauge.and_then(|g| g.apr_from_tvl(now, &|t| app.token_usd(t), pool.tvl_usd));
        // A launch-zone campaign pays on the same LP through its own gauge: shown where the core
        // gauge's APR would be, marked so the two are never read as one number.
        let zone = app.zone_pool_for(&pool.address).filter(|z| z.active(now));
        let zone_apr = zone.and_then(|z| app.zone_apr(z, pool.tvl_usd));
        let mut row = vec![Span::styled(if focused { "▌" } else { " " }, Style::default().fg(t.focus))];
        row.extend(pair_icons(app, t, &pool.token0, &pool.token1));
        row.extend([
            Span::styled(format!("{:<16}", truncate(&name, 16)), style),
            Span::styled(format!("{:>9}  ", pool.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_else(|| "—".into())), t.dim_style()),
            Span::styled(
                format!(
                    "{:>6}",
                    if gauge.is_some() {
                        apr_text(apr)
                    } else if zone.is_some() {
                        apr_text(zone_apr)
                    } else {
                        String::new()
                    }
                ),
                Style::default().fg(if gauge.is_some() { t.ok } else { t.attention }),
            ),
            Span::styled(if zone.is_some() { " ◈" } else { "  " }, Style::default().fg(t.attention)),
            Span::styled(if held { "◆" } else { "" }, Style::default().fg(t.ok)),
        ]);
        lines.push(Line::from(row));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The two tokens of a pair as inline icons, five cells wide whatever the terminal can draw:
/// bitmaps where kitty allows them, tinted monograms otherwise.
fn pair_icons(
    app: &App,
    t: &Theme,
    token0: &wallet_core::markets::PoolToken,
    token1: &wallet_core::markets::PoolToken,
) -> Vec<Span<'static>> {
    use super::images::asset_span;
    // Wrapped QUAI wears the QUAI logo: the pair reads as what it trades, not as its plumbing.
    let (c0, c1) = (app.pool_icon_contract(token0), app.pool_icon_contract(token1));
    vec![asset_span(app, t, &c0, &token0.symbol), Span::raw(" "), asset_span(app, t, &c1, &token1.symbol), Span::raw(" ")]
}
