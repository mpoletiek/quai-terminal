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
use unicode_width::UnicodeWidthStr;
use wallet_core::blocks::BlockHead;
use wallet_core::config::Motion;

/// How long the newest block stays lit after it lands.
const LIT_MS: u128 = 1500;
/// A prime block's moment on the lattice's top line.
const MOMENT_MS: u128 = 1200;

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

/// Heights holding one of this wallet's transactions, from its activity.
fn yours(app: &App) -> std::collections::HashSet<u64> {
    app.dash.activity.iter().filter_map(|a| a.block).collect()
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
    scanline(app, f.buffer_mut(), area, t);
    if hint {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!(
                    "{}public RPC, {} · m reads from your own node instead",
                    t.lead(Icon::Info),
                    if app.eco.chain.live { "new heads by subscription" } else { "read every 2 s" }
                ),
                t.dim_style(),
            )),
            foot,
        );
    }
}

/// What the screen says before its first block, in a panel's inner area.
fn waiting(f: &mut Frame, app: &App, t: &Theme, inner: Rect) {
    let text = match app.eco.chain.error() {
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
    let mine = yours(app);
    let buf = f.buffer_mut();
    let lanes = [("PRIME", 0u8, t.danger), ("REGION", 1u8, t.qi), ("ZONE", 2u8, t.strong)];
    for (i, (name, _, colour)) in lanes.iter().enumerate() {
        buf.set_string(inner.x, inner.y + i as u16 * 2, name, Style::default().fg(*colour));
    }
    let start = inner.x + GUTTER + ((slots - (newest.height - first + 1)) * 2) as u16;
    let x_of = |h: u64| start + ((h - first) * 2) as u16;
    let drawing = Rect { x: inner.x + GUTTER, y: inner.y, width: slots as u16 * 2, height: 5 };
    let picture = LatticePicture {
        slots: slots as u16,
        offset: (start - drawing.x) / 2,
        blocks: (first..=newest.height)
            .map(|h| by_height.get(&h).map(|b| (b.order, b.txs.min(120) as u8, b.workshares.min(24) as u8, mine.contains(&h))))
            .collect(),
        lit: app
            .eco
            .chain
            .arrived
            .get(&newest.height)
            .map(|at| at.elapsed().as_millis())
            .filter(|ms| *ms < LIT_MS && app.motion().effects())
            .map(|ms| (ms / 125) as u8),
    };
    if lattice_pixels(app, buf, t, drawing, picture) {
        draw_moment(app, t, buf, area, newest);
        draw_lattice_numbers(t, buf, inner, label_w, newest);
        return;
    }
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
            let style = if lit == Some(h) || (lane == 2 && mine.contains(&h)) {
                t.strong_style().fg(t.focus)
            } else {
                Style::default().fg(*colour)
            };
            let icon = if lane == 2 && mine.contains(&h) { Icon::Yours } else { order_icon(*order) };
            buf.set_string(x, y, t.icon(icon), style);
            // A tie up from the zone lane through the connector rows to this lane.
            if lane == 2 && b.order < 2 {
                buf.set_string(x, y - 1, "│", Style::default().fg(lanes[b.order as usize].2));
                if b.order == 0 {
                    buf.set_string(x, y - 3, "│", Style::default().fg(lanes[0].2));
                }
            }
        }
    }
    draw_moment(app, t, buf, area, newest);
    draw_lattice_numbers(t, buf, inner, label_w, newest);
}

/// A prime block's moment, on the lattice panel's top line: the block named in the prime hue,
/// stripes to the corner, one colour flip halfway. Region blocks come every few seconds; only
/// prime gets one.
fn draw_moment(app: &App, t: &Theme, buf: &mut ratatui::buffer::Buffer, area: Rect, newest: &BlockHead) {
    if newest.order == 0
        && let Some(ms) = app.eco.chain.arrived.get(&newest.height).map(|at| at.elapsed().as_millis())
        && ms < MOMENT_MS
        && app.motion().effects()
    {
        let title_end = area.x + 2 + titled("lattice", "lattice").width() as u16 + 2;
        let label = format!(" {}PRIME #{} ", t.lead(Icon::Prime), amount::group_thousands(&newest.prime.to_string()));
        let style = if ms < MOMENT_MS / 2 { t.strong_style().fg(t.danger) } else { Style::default().fg(t.on_danger).bg(t.danger) };
        let end = area.right().saturating_sub(2);
        if title_end + (label.width() as u16) < end {
            buf.set_string(title_end, area.y, &label, style);
            for x in title_end + label.width() as u16..end {
                buf.set_string(x, area.y, "▞", Style::default().fg(t.danger));
            }
        }
    }
}

/// Each chain's newest number at the right end of its lane.
fn draw_lattice_numbers(t: &Theme, buf: &mut ratatui::buffer::Buffer, inner: Rect, label_w: u16, newest: &BlockHead) {
    let right = inner.right() - label_w + 1;
    for (lane, n) in [(0u16, newest.prime), (1, newest.region), (2, newest.height)] {
        buf.set_string(right, inner.y + lane * 2, format!("#{}", amount::group_thousands(&n.to_string())), t.dim_style());
    }
}

/// What the pixel lattice draws: per slot from the oldest shown, the block's order, transactions,
/// workshares and whether it holds one of this wallet's transactions (none for a height never
/// read), where the first one sits, and how far into its light the newest block is (eighths of a
/// second).
#[derive(Hash)]
struct LatticePicture {
    slots: u16,
    offset: u16,
    blocks: Vec<Option<(u8, u8, u8, bool)>>,
    lit: Option<u8>,
}

fn rgb3(c: Color) -> Option<[u8; 3]> {
    match c {
        Color::Rgb(r, g, b) => Some([r, g, b]),
        _ => None,
    }
}

/// The lattice in pixels (kitty and Ghostty): anti-aliased lanes and ties, prime and region
/// blocks glowing in their chains' hues, zone blocks sized by the transactions they carried, a
/// tick under each for its workshares, and the newest block's light rising up its tie as it
/// lands. False where it cannot be drawn (another tier, a theme without RGB colours, nothing
/// drawn yet), and the cells draw the lattice instead.
fn lattice_pixels(app: &App, buf: &mut ratatui::buffer::Buffer, t: &Theme, area: Rect, p: LatticePicture) -> bool {
    use super::super::raster::{self, Canvas};
    if !images::bitmaps(app) {
        return false;
    }
    let (Some(prime), Some(region), Some(zone), Some(line), Some(lit)) =
        (rgb3(t.danger), rgb3(t.qi), rgb3(t.strong), rgb3(t.dim), rgb3(t.focus))
    else {
        return false;
    };
    let key = raster::key_of(&(&p, prime, region, zone, line, lit, area.width, area.height));
    raster::scene(app, buf, area, t, "lattice", key, move |c: &mut Canvas| {
        let (cw, ch) = (c.w as f64 / f64::from(p.slots * 2), c.h as f64 / 5.0);
        let lane_y = |lane: u8| (f64::from(lane) * 2.0 + 0.5) * ch;
        let x_of = |i: usize| ((f64::from(p.offset) + i as f64) * 2.0 + 0.5) * cw;
        let colours = [prime, region, zone];
        let width = (ch * 0.07).max(1.0);
        // Lanes: each chain's blocks linked to the next one along; a dotted gap where a zone
        // height was never read.
        for lane in 0..3u8 {
            let mut last: Option<f64> = None;
            for (i, b) in p.blocks.iter().enumerate() {
                match b {
                    Some((order, ..)) if *order <= lane => {
                        if let Some(from) = last {
                            c.line(
                                (from, lane_y(lane)),
                                (x_of(i), lane_y(lane)),
                                width,
                                if lane == 2 { line } else { colours[lane as usize] },
                                0.55,
                            );
                        }
                        last = Some(x_of(i));
                    }
                    None if lane == 2 => {
                        for k in 0..4 {
                            c.dot(x_of(i) - cw + f64::from(k) * cw * 0.66, lane_y(2), width * 0.6, line, 0.6);
                        }
                        last = None;
                    }
                    _ => {}
                }
            }
        }
        for (i, b) in p.blocks.iter().enumerate() {
            let Some((order, txs, ws, yours)) = *b else { continue };
            let x = x_of(i);
            // A tie from the zone lane up to the highest chain the block is also a block of.
            if order < 2 {
                c.line((x, lane_y(2)), (x, lane_y(order)), width, colours[order as usize], 0.7);
            }
            for lane in order..3 {
                let r = match lane {
                    0 => cw * 0.36,
                    1 => cw * 0.30,
                    _ => cw * (0.24 + 0.18 * f64::from(txs) / 120.0),
                };
                if lane < 2 {
                    c.glow(x, lane_y(lane), r * 3.0, colours[lane as usize], 0.30);
                }
                // Yours: a ring in the focus colour around the zone block.
                if lane == 2 && yours {
                    c.dot(x, lane_y(2), r + cw * 0.16, lit, 0.95);
                }
                c.dot(x, lane_y(lane), r, colours[lane as usize], 0.95);
            }
            // Workshares hang under the zone block.
            if ws > 0 {
                let top = lane_y(2) + cw * 0.4;
                c.line((x, top), (x, top + (ch * 0.42 - cw * 0.4).max(2.0) * f64::from(ws) / 24.0), width * 0.8, line, 0.8);
            }
        }
        // The newest block's light: a glow on it, rising up its tie to its chain as it fades.
        if let (Some(q), Some(Some((order, ..)))) = (p.lit, p.blocks.last()) {
            let k = f64::from(q) / 12.0;
            let x = x_of(p.blocks.len() - 1);
            let top = lane_y(*order);
            let y = lane_y(2) + (top - lane_y(2)) * (k * 1.6).min(1.0);
            c.glow(x, y, cw * 1.6, lit, 0.8 * (1.0 - k));
            c.dot(x, y, cw * 0.22, lit, 1.0 - k);
        }
    })
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
    // One row each, label then sparkline; a row between them where the panel is tall enough.
    let step = if inner.height >= 5 { 2 } else { 1 };
    for (i, (name, pick, colour)) in algos.iter().enumerate() {
        let y = inner.y + i as u16 * step;
        if y >= inner.bottom() {
            break;
        }
        let label = format!("{name:<7}{:>11} ", wallet_core::chainstats::hashrate_text(pick(&s.hashrate)));
        let label_w = (label.chars().count() as u16).min(inner.width);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{name:<7}"), t.dim_style()),
                Span::styled(label.chars().skip(7).collect::<String>(), t.strong_style()),
            ])),
            Rect { y, height: 1, width: label_w, ..inner },
        );
        // Log scale, then the window's own range: a few percent of movement fills the row.
        let logs: Vec<f64> = s.hashrate_history.iter().map(|(_, h)| pick(h).max(1.0).log10()).collect();
        let lo = logs.iter().copied().fold(f64::INFINITY, f64::min);
        let data: Vec<u64> = logs.iter().map(|v| ((v - lo) * 1000.0) as u64 + 1).collect();
        let row = Rect { x: inner.x + label_w, y, width: inner.width - label_w, height: 1 };
        f.render_widget(
            Sparkline::default().data(super::super::ui::screens::scaled(&data, row.width)).style(Style::default().fg(*colour)),
            row,
        );
    }
}

/// Base fee per block, the newest in the title, and above the bars how much of the fee policy's
/// gas price it is. A base fee above the policy turns the bars red and says so: highlighted,
/// never blocked (a review over policy still goes through, see `fee_policy_note`).
fn draw_base_fee(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let blocks = window(app, area.width.saturating_sub(4));
    let policy = app.config.network(&app.network_id).ok().and_then(|n| n.max_gas_price.parse::<u128>().ok()).filter(|p| *p > 0);
    let newest = blocks.last().map(|b| b.base_fee_wei);
    let over = newest.zip(policy).is_some_and(|(fee, cap)| fee > cap);
    let last = newest.map(|fee| format!(" · {}", gwei_text(fee as f64 / 1e9))).unwrap_or_default();
    let block = panel(t, &format!("{}{last}", titled("gas", "base fee")), false);
    let mut inner = block.inner(area);
    f.render_widget(block, area);
    if blocks.is_empty() {
        return waiting(f, app, t, inner);
    }
    if let (Some(fee), Some(cap)) = (newest, policy)
        && inner.height > 2
    {
        let (text, style) = match over {
            true => (format!("above your fee policy ({})", gwei_text(cap as f64 / 1e9)), Style::default().fg(t.danger)),
            false => (format!("{}% of your fee policy ({})", fee * 100 / cap, gwei_text(cap as f64 / 1e9)), t.dim_style()),
        };
        f.render_widget(Paragraph::new(Span::styled(text, style)), Rect { height: 1, ..inner });
        inner = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    }
    let colour = if over { t.danger } else { t.attention };
    let lo = blocks.iter().map(|b| b.base_fee_wei).min().unwrap_or(0);
    let data: Vec<u64> = blocks.iter().map(|b| ((b.base_fee_wei - lo) / 1_000_000) as u64 + 1).collect();
    f.render_widget(Sparkline::default().data(&data).style(Style::default().fg(colour)), inner);
    super::super::edge::ramp_bars(app, f.buffer_mut(), inner, colour, t);
}

/// Every block read, newest first.
fn draw_feed(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let how = match (app.eco.chain.live, app.eco.chain.watch.error()) {
        (true, _) => " · live",
        (false, Some(_)) => " · polled (no WebSocket)",
        (false, None) => "",
    };
    let block = panel(t, &format!("{}{how}", titled("blocks", "blocks")), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.chain.blocks.is_empty() {
        return waiting(f, app, t, inner);
    }
    let now = wallet_core::registry::now_f64() as u64;
    let lit = lit(app);
    let mine = yours(app);
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
                Cell::from(Span::styled(if mine.contains(&b.height) { "▌" } else { " " }, Style::default().fg(t.focus))),
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
        Constraint::Length(1),
        Constraint::Length(11),
        Constraint::Length(6),
        Constraint::Length(9),
        Constraint::Length(13),
        Constraint::Length(5),
        Constraint::Length(5),
        Constraint::Length(6),
        Constraint::Length(5),
    ];
    let mut header = vec!["", "height", "age", "order", "hash", "txs", "out", "gas", "bits"];
    if wide {
        widths.extend([Constraint::Length(5), Constraint::Length(12), Constraint::Min(13)]);
        header.extend(["ws", "base fee", "miner"]);
    }
    f.render_widget(Table::new(rows, widths).header(Row::new(header).style(t.dim_style())), inner);
}

/// A band of light that sweeps down the screen, one row a tenth of a second, and rests a moment
/// at the bottom. Vivid only.
fn scanline(app: &App, buf: &mut ratatui::buffer::Buffer, area: Rect, t: &Theme) {
    if app.motion() != Motion::Vivid || !app.term.focused || !app.term.caps.truecolor {
        return;
    }
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    scanline_at(buf, area, t, ms);
}

/// The scanline `ms` into the clock: only behind blank cells and panel lines, so text never
/// changes and no height, hash, fee or address moves under it.
fn scanline_at(buf: &mut ratatui::buffer::Buffer, area: Rect, t: &Theme, ms: u128) {
    if area.height < 4 {
        return;
    }
    let sweep = u128::from(area.height) * 100;
    let ms = ms % (sweep + 1500);
    if ms >= sweep {
        return;
    }
    let row = area.y + (ms / 100) as u16;
    for (y, k) in [(row, 0.10), (row.saturating_sub(1), 0.05)] {
        if y < area.y {
            continue;
        }
        let Some(bg) = super::super::edge::tint(t, t.focus, k) else { return };
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            if matches!(cell.symbol(), " " | "─" | "│" | "┌" | "┐" | "└" | "┘" | "┈") {
                cell.set_bg(bg);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;

    #[test]
    fn the_scanline_lights_blank_cells_and_lines_but_never_text() {
        let dir = tempfile::tempdir().unwrap();
        let t = super::super::super::theme::resolve(dir.path(), "quai-red", false, false).0;
        let area = Rect::new(0, 0, 30, 10);
        let mut plain = Buffer::empty(area);
        plain.set_string(0, 3, "fee 63,607 gwei 0x0011…85b8", t.text_style());
        plain.set_string(0, 2, "┌──────┐", t.dim_style());
        let mut lit = plain.clone();
        // 300 ms in: the band is on row 3, its trail on row 2.
        scanline_at(&mut lit, area, &t, 300);
        let text = (0..30u16).filter(|x| plain[(*x, 3)].symbol() != " ");
        for x in text {
            assert_eq!(lit[(x, 3)], plain[(x, 3)], "text at {x} untouched");
        }
        assert_ne!(lit[(29, 3)].bg, plain[(29, 3)].bg, "a blank cell on the band is lit");
        assert_ne!(lit[(1, 2)].bg, plain[(1, 2)].bg, "and a panel line on its trail");
        let mut rest = plain.clone();
        scanline_at(&mut rest, area, &t, 1_200);
        assert_eq!(rest, plain, "resting at the bottom");
    }
}
