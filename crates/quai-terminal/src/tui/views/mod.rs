//! Views for the ecosystem sections: Home, the exchange cards, NFTs, data
//! sources and the detail stack. Shared widgets come from `ui`.

use super::app::{self, App, Detail, Screen, Section};
use super::eco::WRAP_MODES;
use super::icons::Icon;
use super::images;
use super::num;
use super::theme::Theme;
use super::ui::{
    activity_table_rows, ago, ago_short, big_digits, empty, empty_state, hero_fits, incoming_text, panel, spinner, status_glyph, truncate,
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

pub(crate) mod board;
mod cards;
mod data_sources;
mod home;
mod markets;
mod nfts;
mod pools;
mod wallets;
pub use board::*;
pub use cards::*;
pub use data_sources::*;
pub use home::*;
pub use markets::*;
pub use nfts::*;
pub use pools::*;
pub use wallets::*;

pub(crate) const BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

pub(crate) fn key(t: &Theme, k: &str) -> Span<'static> {
    Span::styled(format!("{k} "), t.strong_style().fg(t.focus))
}

pub(crate) fn kv<'a>(t: &Theme, k: &str, v: impl Into<Span<'a>>) -> Line<'a> {
    super::widgets::kv(t, k, vec![v.into()])
}

/// A `kv` line with an icon in front of the value.
pub(crate) fn kv_icon<'a>(t: &Theme, k: &str, icon: Span<'static>, v: impl Into<Span<'a>>) -> Line<'a> {
    super::widgets::kv(t, k, vec![icon, Span::raw(" "), v.into()])
}

/// Icon for a listing's payment currency (native QUAI or a token the network knows).
pub(crate) fn currency_span(app: &App, t: &Theme, l: &wallet_core::market::Listing) -> Option<Span<'static>> {
    if l.is_native() {
        return Some(images::native_span(app, t, "quai"));
    }
    let network = app.net()?;
    let (symbol, _) = wallet_core::market::known_currency(&network, &l.currency)?;
    Some(images::asset_span(app, t, &l.currency, symbol))
}

pub(crate) fn trust_span(t: &Theme, trust: Trust) -> Span<'static> {
    match trust {
        Trust::Verified => Span::styled(t.icon(Icon::Ok), Style::default().fg(t.ok)),
        Trust::Unverified => Span::styled(t.icon(Icon::Warning), Style::default().fg(t.attention)),
        Trust::Unknown => Span::styled("·", t.dim_style()),
    }
}

pub(crate) fn src_span(t: &Theme, r: &AssetRow, stale: bool) -> Span<'static> {
    let (glyph, style) = match r.price_kind {
        PriceKind::Market => (t.icon(Icon::On), Style::default().fg(t.ok)),
        PriceKind::Protocol => (t.icon(Icon::Protocol), Style::default().fg(t.qi)),
        PriceKind::None if r.trust == Trust::Unverified => (t.icon(Icon::Warning), Style::default().fg(t.attention)),
        PriceKind::None => ("—", t.dim_style()),
    };
    let age = wallet_core::registry::now().saturating_sub(r.price_at);
    if stale || (r.price_at > 0 && age > 900) {
        Span::styled(format!("{} {}", t.icon(Icon::Stale), wallet_core::track::human_duration(age)), t.dim_style())
    } else {
        Span::styled(glyph, style)
    }
}

/// A holding's balance, `~` in front when it is approximate (a column of them lines up on the
/// decimal point through [`num::align`]).
pub(crate) fn balance_text(r: &AssetRow) -> String {
    let text = amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 4));
    if r.exact { text } else { format!("~{text}") }
}

/// QUAI keeps its blue and Qi its magenta; tokens take their icon's color (contrast-guarded).
pub(crate) fn asset_color(app: &App, t: &Theme, r: &AssetRow) -> Color {
    match &r.key {
        AssetKey::Quai => t.quai,
        AssetKey::Qi => t.qi,
        AssetKey::Token(a) => images::token_tint(app, t, r.icon_url.as_deref(), &r.symbol, a),
    }
}

pub(crate) fn contract_of(r: &AssetRow) -> String {
    match &r.key {
        AssetKey::Token(a) => a.clone(),
        k => k.id(),
    }
}

/// One-line activity description for token/NFT transfer rows and receipts.
pub fn activity_text(a: &Activity) -> String {
    if a.detail.sale().as_bool() == Some(true) {
        let name = a.detail.name().as_str().unwrap_or("NFT");
        let decimals = a.detail.decimals().as_u64().unwrap_or(18) as u8;
        let v: U256 = a.amount.parse().unwrap_or_default();
        return format!(
            "sold {name} for {} {}",
            amount::group_thousands(&amount::format_amount_short(v, decimals, 4)),
            num::unit(&a.asset)
        );
    }
    if a.detail.source().as_str() == Some("explorer") {
        let standard = a.detail.standard().as_str().unwrap_or("ERC-20");
        let verb = if a.direction == "in" { "received" } else { "sent" };
        if standard != "ERC-20" {
            let name = a.detail.name().as_str().filter(|n| !n.is_empty()).unwrap_or(&a.asset);
            return format!("{verb} {name} #{}", a.detail.token_id().as_str().unwrap_or("?"));
        }
        let decimals = a.detail.decimals().as_u64().unwrap_or(18) as u8;
        let v: U256 = a.amount.parse().unwrap_or_default();
        return format!("{verb} {} {}", amount::group_thousands(&amount::format_amount_short(v, decimals, 4)), num::unit(&a.asset));
    }
    format!("{} {}", wallet_core::track::incoming_verb(a), incoming_text(a))
}

// ---------------------------------------------------------------- shell

/// Sub-tab strip at the top of the content area: the labels, and on two rows a rule under them
/// that is lit beneath the open tab.
pub fn draw_tabs(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let section = app.nav.screen.section();
    let active = if section == Section::Activity {
        app::ActivityFilter::ALL.iter().position(|x| *x == app.nav.activity_filter).unwrap_or(0)
    } else {
        section.screens(&app.shown()).iter().position(|s| *s == app.nav.screen).unwrap_or(0)
    };
    let labels = section.tab_labels(&app.shown());
    let mut spans = vec![Span::styled(format!(" {} ", section.title()), t.dim_style()), Span::styled("· ", t.dim_style())];
    // Where the open tab sits along the row, for the lit stretch of the rule.
    let mut lit = (0usize, 0usize);
    for (i, label) in labels.iter().enumerate() {
        let tab = format!(" {label} ");
        if i == active {
            lit = (Line::from(spans.clone()).width(), tab.chars().count());
            spans.push(Span::styled(tab, t.selected()));
        } else {
            spans.push(Span::styled(tab, t.text_style()));
        }
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(" [ ]", t.dim_style()));
    let row = Rect { height: 1, ..area };
    // Each label is its tab: span 2 + 2i (after the section title and the dot), up to the last
    // label; the `[ ]` hint after them is not a tab.
    let tabs = labels.len();
    app.input
        .hits
        .borrow_mut()
        .spans(row, &spans, |i| (i >= 2 && i % 2 == 0 && (i - 2) / 2 < tabs).then(|| crate::tui::hit::Target::Tab((i - 2) / 2)));
    f.render_widget(Paragraph::new(Line::from(spans)), row);
    if area.height >= 2 {
        let width = area.width as usize;
        let (at, len) = (lit.0.min(width), lit.1.min(width.saturating_sub(lit.0)));
        let rule = Line::from(vec![
            Span::styled("─".repeat(at), t.border(false)),
            Span::styled("━".repeat(len), Style::default().fg(t.focus)),
            Span::styled("─".repeat(width - at - len), t.border(false)),
        ]);
        f.render_widget(Paragraph::new(rule), Rect { y: area.y + 1, height: 1, ..area });
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
