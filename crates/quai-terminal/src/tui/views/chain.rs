//! System › Chain: Quai's hierarchy as this zone sees it. A block timer, the prime → region → zone
//! map, the newest head, the block lattice (which zone blocks were also region and prime
//! blocks), per-block entropy, hashrate and base fee, and the block feed.
//!
//! Nothing here is money: heights, hashes, counts and network fees. The newest block is lit for a
//! moment when it lands (above Reduced motion); everything else holds still.

use super::super::glyphfont::seven_seg;
use super::super::kana::titled;
use super::super::ui::screens::{count_text, gwei_text};
use super::*;
use ratatui::widgets::Sparkline;
use wallet_core::blocks::BlockHead;

/// How long the newest block stays lit after it lands.
const LIT_MS: u128 = 1500;

const REGIONS: [&str; 3] = ["Cyprus", "Paxos", "Hydra"];

fn order_icon(order: u8) -> Icon {
    match order {
        0 => Icon::Prime,
        1 => Icon::Region,
        _ => Icon::Zone,
    }
}

/// Prime in the danger hue and region in Qi's, as on the lattice's lanes; zone quiet.
fn order_style(t: &Theme, order: u8) -> Style {
    match order {
        0 => Style::default().fg(t.danger),
        1 => Style::default().fg(t.qi),
        _ => t.dim_style(),
    }
}

fn order_word(order: u8) -> &'static str {
    match order {
        0 => "prime",
        1 => "region",
        _ => "zone",
    }
}

/// The newest block, while it is still lit.
fn lit(app: &App) -> Option<u64> {
    let b = app.eco.chain.newest()?;
    let at = app.eco.chain.arrived.get(&b.height)?;
    (app.motion().effects() && at.elapsed().as_millis() < LIT_MS).then_some(b.height)
}

pub fn draw_chain(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let hint = !app.config.monitor_endpoints.contains_key(&app.dash.network_id);
    let tall = area.height >= 34;
    let [top, lattice, mid, feed, foot] = Layout::vertical([
        Constraint::Length(9),
        Constraint::Length(7),
        Constraint::Length(if tall { 7 } else { 0 }),
        Constraint::Min(3),
        Constraint::Length(u16::from(hint)),
    ])
    .areas(area);
    let wide = top.width >= 124;
    let [timer, map, head] =
        Layout::horizontal([Constraint::Length(30), Constraint::Length(if wide { 44 } else { 0 }), Constraint::Min(30)]).areas(top);
    draw_timer(f, app, t, timer);
    if wide {
        draw_hierarchy(f, app, t, map);
    }
    draw_head(f, app, t, head);
    draw_lattice(f, app, t, lattice);
    if tall {
        let [entropy, hashrate, gas] =
            Layout::horizontal([Constraint::Percentage(34), Constraint::Percentage(36), Constraint::Percentage(30)]).areas(mid);
        draw_entropy(f, app, t, entropy);
        draw_hashrate(f, app, t, hashrate);
        draw_base_fee(f, app, t, gas);
    }
    draw_feed(f, app, t, feed);
    if hint {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("{}public RPC, read every 2 s · m reads from your own node instead", t.lead(Icon::Info)),
                t.dim_style(),
            )),
            foot,
        );
    }
}

/// What the screen says before its first block, in a panel's inner area.
fn waiting(f: &mut Frame, app: &App, t: &Theme, inner: Rect) {
    let text = match &app.eco.chain.error {
        Some(e) => format!("{} {}", t.icon(Icon::Info), truncate(&app::friendly_error(e), inner.width as usize)),
        None => format!("{} reading blocks…", spinner()),
    };
    f.render_widget(Paragraph::new(Span::styled(text, t.dim_style())), inner);
}

/// Seconds since the newest block's timestamp in seven segments, over a gauge that fills at twice
/// three times the average block time. Block times are roughly exponential, so one in twenty
/// blocks takes three times the average (attention colour) and only one in four hundred six times
/// it, when the timer says the block is late.
fn draw_timer(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("timer", "block timer"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(newest) = app.eco.chain.newest() else { return waiting(f, app, t, inner) };
    let since = (wallet_core::registry::now_f64() - newest.timestamp as f64).max(0.0);
    let text = match since {
        s if s >= 100.0 => format!("{}:{:02}", s as u64 / 60, s as u64 % 60),
        s if app.motion().effects() => format!("{s:04.1}"),
        s => format!("{:02}", s as u64),
    };
    let avg = app.eco.chain.avg_block_secs().filter(|a| *a > 0.0).unwrap_or(5.0);
    let late = since > avg * 6.0;
    let colour = if late {
        t.danger
    } else if since > avg * 3.0 {
        t.attention
    } else {
        t.strong
    };
    let mut lines: Vec<Line> = seven_seg(&text).into_iter().map(|r| Line::from(Span::styled(r, Style::default().fg(colour)))).collect();
    let width = inner.width.saturating_sub(8) as usize;
    let filled = ((since / (avg * 3.0)).min(1.0) * width as f64).round() as usize;
    lines.push(Line::from(vec![
        Span::styled("■".repeat(filled), Style::default().fg(if late { t.danger } else { t.ok })),
        Span::styled("□".repeat(width - filled), t.dim_style()),
        Span::styled(if late { " late" } else { "" }, Style::default().fg(t.danger)),
    ]));
    lines.push(Line::from(Span::styled(
        format!("#{} · avg {avg:.1} s", amount::group_thousands(&newest.height.to_string())),
        t.dim_style(),
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// Prime over its three regions over their nine zones, with this wallet's zone lit; the chain the
/// newest block also belongs to lights its path for a moment. Columns of 13 cells, one region and
/// its zones each, prime over the middle one.
fn draw_hierarchy(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("hierarchy", "hierarchy"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let newest = app.eco.chain.newest();
    let flash = lit(app).and(newest).map(|b| b.order);
    let hot = |order: u8| flash.is_some_and(|o| o <= order);
    let style = |on: bool| if on { t.strong_style().fg(t.focus) } else { t.dim_style() };
    let number = |n: Option<u64>| n.map(|n| format!("#{}", amount::group_thousands(&n.to_string()))).unwrap_or_default();
    let o = t.icon(Icon::OtherZone);
    let cols = |parts: [String; 3]| parts.iter().map(|p| format!("{p:<13}")).collect::<String>();
    let lines = vec![
        Line::from(vec![
            Span::raw(" ".repeat(13)),
            Span::styled(format!("{}prime {}", t.lead(Icon::Prime), number(newest.map(|b| b.prime))), style(hot(0))),
        ]),
        Line::from(Span::styled(format!("┌{}┼{}┐", "─".repeat(12), "─".repeat(12)), style(hot(0)))),
        Line::from(vec![
            Span::styled(format!("{:<13}", format!("{}{}", t.lead(Icon::Region), REGIONS[0])), style(hot(1))),
            Span::styled(
                cols([format!("{}{}", t.lead(Icon::Region), REGIONS[1]), format!("{}{}", t.lead(Icon::Region), REGIONS[2]), String::new()]),
                t.dim_style(),
            ),
        ]),
        Line::from(Span::styled(number(newest.map(|b| b.region)), t.dim_style())),
        Line::from(vec![
            Span::styled(t.icon(Icon::Zone).to_string(), t.strong_style().fg(t.focus)),
            Span::styled(format!("{:<12}", format!(" {o} {o}")), t.dim_style()),
            Span::styled(cols([format!("{o} {o} {o}"), format!("{o} {o} {o}"), String::new()]), t.dim_style()),
        ]),
        Line::from(Span::styled(cols(["1 2 3".into(), "1 2 3".into(), "1 2 3".into()]), t.dim_style())),
        Line::from(vec![
            Span::styled(t.lead(Icon::Zone), t.strong_style().fg(t.focus)),
            Span::styled("Cyprus-1, this wallet's zone", t.dim_style()),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// The newest block: height, hash, which chains it belongs to, and what it carried.
fn draw_head(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("head", "head"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(b) = app.eco.chain.newest() else { return waiting(f, app, t, inner) };
    let kv = |k: &str, v: Vec<Span<'static>>| super::super::widgets::kv(t, k, v);
    let gas = if b.gas_limit > 0 { b.gas_used as f64 / b.gas_limit as f64 * 100.0 } else { 0.0 };
    let lines = vec![
        kv(
            "height",
            vec![
                Span::styled(amount::group_thousands(&b.height.to_string()), t.strong_style()),
                Span::styled(format!("  {} {}", t.icon(order_icon(b.order)), order_word(b.order)), order_style(t, b.order)),
            ],
        ),
        kv("hash", vec![Span::raw(short_address(&b.hash))]),
        kv(
            "prime · region",
            vec![Span::raw(format!(
                "#{} · #{}",
                amount::group_thousands(&b.prime.to_string()),
                amount::group_thousands(&b.region.to_string())
            ))],
        ),
        kv("carried", vec![Span::raw(format!("{} txs · {} out to zones · {} workshares", b.txs, b.etxs, b.workshares))]),
        kv("gas used", vec![Span::raw(format!("{gas:.1}% of {}", count_text(b.gas_limit)))]),
        kv("base fee", vec![Span::raw(gwei_text(b.base_fee_wei as f64 / 1e9))]),
        kv("entropy", vec![Span::raw(format!("+{} bits to the zone's total", bits(b)))]),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}

/// One lane per chain, newest on the right, two cells a block. Every block is a zone block; one
/// that is also a region (or prime) block is tied up to that lane. A height this screen never
/// read is a dotted gap.
fn draw_lattice(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("lattice", "lattice"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let log = &app.eco.chain;
    let Some(newest) = log.newest() else { return waiting(f, app, t, inner) };
    if inner.height < 5 || inner.width < 30 {
        return;
    }
    const GUTTER: u16 = 8;
    let label_w = 12u16;
    let slots = ((inner.width - GUTTER - label_w) / 2) as u64;
    // Newest on the right; nothing drawn left of the oldest block read.
    let oldest = log.blocks.front().map_or(newest.height, |b| b.height);
    let first = newest.height.saturating_sub(slots.saturating_sub(1)).max(oldest);
    let by_height: std::collections::HashMap<u64, &BlockHead> =
        log.blocks.iter().filter(|b| b.height >= first).map(|b| (b.height, b)).collect();
    let lit = lit(app);
    let buf = f.buffer_mut();
    let lanes = [("PRIME", 0u8, t.danger), ("REGION", 1u8, t.qi), ("ZONE", 2u8, t.strong)];
    for (i, (name, _, colour)) in lanes.iter().enumerate() {
        buf.set_string(inner.x, inner.y + i as u16 * 2, name, Style::default().fg(*colour));
    }
    let start = inner.x + GUTTER + ((slots - (newest.height - first + 1)) * 2) as u16;
    let x_of = |h: u64| start + ((h - first) * 2) as u16;
    for (lane, (_, order, colour)) in lanes.iter().enumerate() {
        let y = inner.y + lane as u16 * 2;
        // Links: from each block of this chain to the next one along.
        let mut last: Option<u16> = None;
        for h in first..=newest.height {
            let Some(b) = by_height.get(&h) else {
                if lane == 2 {
                    buf.set_string(x_of(h), y, "┈┈", t.dim_style());
                    last = None;
                }
                continue;
            };
            if b.order <= *order {
                let x = x_of(h);
                if let Some(from) = last {
                    for lx in from + 1..x {
                        buf.set_string(lx, y, "─", t.dim_style());
                    }
                }
                last = Some(x);
            }
        }
        for h in first..=newest.height {
            let Some(b) = by_height.get(&h) else { continue };
            if b.order > *order {
                continue;
            }
            let x = x_of(h);
            let style = if lit == Some(h) { t.strong_style().fg(t.focus) } else { Style::default().fg(*colour) };
            buf.set_string(x, y, t.icon(order_icon(*order)), style);
            // A tie up from the zone lane through the connector rows to this lane.
            if lane == 2 && b.order < 2 {
                buf.set_string(x, y - 1, "│", Style::default().fg(lanes[b.order as usize].2));
                if b.order == 0 {
                    buf.set_string(x, y - 3, "│", Style::default().fg(lanes[0].2));
                }
            }
        }
    }
    let right = inner.right() - label_w + 1;
    for (lane, n) in [(0u16, newest.prime), (1, newest.region), (2, newest.height)] {
        buf.set_string(right, inner.y + lane * 2, format!("#{}", amount::group_thousands(&n.to_string())), t.dim_style());
    }
}

/// The blocks the lattice shows, oldest first, for the strip charts under it.
fn window(app: &App, width: u16) -> Vec<&BlockHead> {
    let n = width as usize;
    let blocks = &app.eco.chain.blocks;
    blocks.iter().skip(blocks.len().saturating_sub(n)).collect()
}

/// Entropy a block's work added, in bits, as a string with one decimal.
fn bits(b: &BlockHead) -> String {
    format!("{}.{}", b.entropy_mbits / 1000, b.entropy_mbits % 1000 / 100)
}

/// The entropy each block added to the zone's total (PoEM weighs chains by it), each bar from
/// the window's least.
fn draw_entropy(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let blocks = window(app, area.width.saturating_sub(4));
    let best = blocks.iter().max_by_key(|b| b.entropy_mbits).map(|b| format!(" · best {} bits", bits(b))).unwrap_or_default();
    let block = panel(t, &format!("{}{best}", titled("entropy", "entropy added")), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if blocks.is_empty() {
        return waiting(f, app, t, inner);
    }
    let floor = blocks.iter().map(|b| b.entropy_mbits).min().unwrap_or(0).saturating_sub(250);
    let data: Vec<u64> = blocks.iter().map(|b| b.entropy_mbits - floor).collect();
    f.render_widget(Sparkline::default().data(&data).style(Style::default().fg(t.qi)), inner);
    super::super::edge::ramp_bars(app, f.buffer_mut(), inner, t.qi, t);
}

/// Hashrate by algorithm over the last day, each on a log scale of its own.
fn draw_hashrate(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("hashrate", "hashrate · 24h"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(s) = app.eco.feeds.chain_stats.value().filter(|s| !s.hashrate_history.is_empty()) else {
        return super::super::ui::screens::chart_placeholder(f, app, t, inner);
    };
    type Pick = fn(&wallet_core::chainstats::Hashrates) -> f64;
    let algos: [(&str, Pick, Color); 3] =
        [("SHA", |h| h.sha, t.chart[0]), ("Scrypt", |h| h.scrypt, t.chart[2]), ("KawPoW", |h| h.kawpow, t.chart[4])];
    for (i, (name, pick, colour)) in algos.iter().enumerate() {
        let y = inner.y + i as u16 * 2;
        if y + 1 >= inner.bottom() {
            break;
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{name:<7}"), t.dim_style()),
                Span::styled(wallet_core::chainstats::hashrate_text(pick(&s.hashrate)), t.strong_style()),
            ])),
            Rect { y, height: 1, ..inner },
        );
        // Log scale, then the window's own range: a few percent of movement fills the row.
        let logs: Vec<f64> = s.hashrate_history.iter().map(|(_, h)| pick(h).max(1.0).log10()).collect();
        let lo = logs.iter().copied().fold(f64::INFINITY, f64::min);
        let data: Vec<u64> = logs.iter().map(|v| ((v - lo) * 1000.0) as u64 + 1).collect();
        let row = Rect { y: y + 1, height: 1, ..inner };
        f.render_widget(
            Sparkline::default().data(super::super::ui::screens::scaled(&data, row.width)).style(Style::default().fg(*colour)),
            row,
        );
    }
}

/// Base fee per block, in gwei, with the newest in the title.
fn draw_base_fee(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let blocks = window(app, area.width.saturating_sub(4));
    let last = blocks.last().map(|b| format!(" · {}", gwei_text(b.base_fee_wei as f64 / 1e9))).unwrap_or_default();
    let block = panel(t, &format!("{}{last}", titled("gas", "base fee")), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if blocks.is_empty() {
        return waiting(f, app, t, inner);
    }
    let lo = blocks.iter().map(|b| b.base_fee_wei).min().unwrap_or(0);
    let data: Vec<u64> = blocks.iter().map(|b| ((b.base_fee_wei - lo) / 1_000_000) as u64 + 1).collect();
    f.render_widget(Sparkline::default().data(&data).style(Style::default().fg(t.attention)), inner);
    super::super::edge::ramp_bars(app, f.buffer_mut(), inner, t.attention, t);
}

/// Every block read, newest first.
fn draw_feed(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let block = panel(t, &titled("blocks", "blocks"), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.chain.blocks.is_empty() {
        return waiting(f, app, t, inner);
    }
    let now = wallet_core::registry::now_f64() as u64;
    let lit = lit(app);
    let wide = inner.width >= 100;
    let rows: Vec<Row> = app
        .eco
        .chain
        .blocks
        .iter()
        .rev()
        .take(inner.height.saturating_sub(1) as usize)
        .map(|b| {
            let gas = if b.gas_limit > 0 { b.gas_used as f64 * 100.0 / b.gas_limit as f64 } else { 0.0 };
            let order = Span::styled(format!("{} {}", t.icon(order_icon(b.order)), order_word(b.order)), order_style(t, b.order));
            let mut cells = vec![
                Cell::from(Span::styled(amount::group_thousands(&b.height.to_string()), t.strong_style())),
                Cell::from(format!("{}s", now.saturating_sub(b.timestamp))),
                Cell::from(order),
                Cell::from(short_address(&b.hash)),
                Cell::from(b.txs.to_string()),
                Cell::from(b.etxs.to_string()),
                Cell::from(format!("{gas:.1}%")),
                Cell::from(bits(b)),
            ];
            if wide {
                cells.push(Cell::from(b.workshares.to_string()));
                cells.push(Cell::from(gwei_text(b.base_fee_wei as f64 / 1e9)));
                cells.push(Cell::from(Span::styled(short_address(&b.miner), t.dim_style())));
            }
            let row = Row::new(cells);
            if lit == Some(b.height) { row.style(Style::default().bg(t.selection)) } else { row }
        })
        .collect();
    let mut widths = vec![
        Constraint::Length(11),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(5),
    ];
    let mut header = vec!["height", "age", "order", "hash", "txs", "out", "gas", "bits"];
    if wide {
        widths.extend([Constraint::Length(5), Constraint::Length(12), Constraint::Min(13)]);
        header.extend(["ws", "base fee", "miner"]);
    }
    f.render_widget(Table::new(rows, widths).header(Row::new(header).style(t.dim_style())), inner);
}
