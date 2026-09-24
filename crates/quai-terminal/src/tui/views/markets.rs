//! Trade › Markets: the pairs, the DEX-wide flow and a pair's chart and tape.

use super::*;

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

/// A bonded curve's TVL: both sides of its locked pool, the token side at the pool's own price,
/// in USD when QUAI has a price and in QUAI when it does not.
fn locked_tvl(app: &App, pool: &wallet_core::markets::Pool, curve: &wallet_core::markets::CurveMark) -> String {
    let quai = 2.0 * curve.locked_quai.unwrap_or(0.0);
    match app.token_usd(&pool.token1) {
        Some(usd) => wallet_core::swap::usd_compact(quai * usd),
        None => format!("{}Q", compact(quai)),
    }
}

fn compact(v: f64) -> String {
    if v >= 1e6 {
        format!("{:.1}M", v / 1e6)
    } else if v >= 1e3 {
        format!("{:.1}k", v / 1e3)
    } else {
        format!("{v:.0}")
    }
}

pub(crate) fn pct_span(t: &Theme, pct: Option<f64>) -> Span<'static> {
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
            Span::styled(
                format!("{}{n}%", t.icon(if c > 0.0 { Icon::Up } else { Icon::Down })),
                Style::default().fg(if c > 0.0 { t.up } else { t.down }),
            )
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
            return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]);
        }
        Some(Ok((p, o))) => (p, o),
    };
    if pools.is_empty() {
        let block = panel(t, "markets · Quainance", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty(f, inner, t, Icon::Pool, "No pools on this network.", &[]);
    }
    let now = wallet_core::registry::now();
    // What the filters keep. The chart, the flow and the alerts all follow this list.
    let rows_pools = app.market_rows();
    let selected = app.markets_pair().min(rows_pools.len().saturating_sub(1));
    let wide = area.width >= 110;
    // The left column carries the pairs and, under them, the DEX-wide flow; the chart keeps its
    // own width. A narrow terminal stacks them and only shows the flow when there is height for it.
    let (list_area, main, flow_area) = if wide {
        // 134, so a 160-column terminal keeps the wide column beside the section rail.
        let column = if area.width >= 134 { 54 } else { 46 };
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
        super::super::eco::MarketSort::Default => String::new(),
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
    let block = panel(t, &title, app.screen == Screen::Markets && app.lit_pane() == Some(0));
    let inner = block.inner(list_area);
    f.render_widget(block, list_area);
    let visible = inner.height.saturating_sub(1) as usize;
    let pairs_id = crate::tui::hit::ListId::Screen(Screen::Markets, 0);
    let offset = app.list_window(pairs_id, selected, rows_pools.len(), visible);
    {
        let mut hits = app.hits.borrow_mut();
        hits.add(list_area, crate::tui::hit::Target::Pane(0));
        let body = Rect { y: inner.y + 1, height: visible as u16, ..inner };
        hits.rows(pairs_id, body, offset, rows_pools.len(), |i| rows_pools.get(i).map(|p| p.address.clone()));
    }
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
                Venue::LaunchAmm => Span::styled(format!(" {}", t.icon(Icon::Launch)), Style::default().fg(t.link)),
                Venue::Curve => Span::styled(format!(" {}", t.icon(Icon::Curve)), Style::default().fg(t.attention)),
                Venue::Legacy => Span::styled(format!(" {}", t.icon(Icon::Legacy)), Style::default().fg(t.attention)),
                Venue::HartiiAmm => Span::styled(format!(" {}", t.icon(Icon::Hartii)), Style::default().fg(t.link)),
                Venue::Main => Span::raw(""),
            };
            // Watched pairs sit at the top, marked.
            let watched = app.eco.watchlist.iter().any(|w| w.eq_ignore_ascii_case(&p.address));
            let watch =
                Span::styled(if watched { format!(" {}", t.icon(Icon::On)) } else { String::new() }, Style::default().fg(t.attention));
            let depth = match &p.curve {
                // A bonded curve's pool is locked for good, so its depth is its TVL, not "100%".
                Some(c) if c.locked_quai.is_some() => Span::styled(locked_tvl(app, p, c), t.dim_style()),
                Some(c) => Span::styled(format!("{}%", c.progress_bps.unwrap_or(0) / 100), Style::default().fg(t.attention)),
                None => Span::styled(p.tvl_usd.map(wallet_core::swap::usd_compact).unwrap_or_default(), t.dim_style()),
            };
            let row = Row::new(vec![
                Cell::from(Line::from(vec![
                    // A pair's icons: two cells each, one cell apart (as `pair_icons` draws them).
                    base_icon,
                    Span::raw(" "),
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
        "{base_sym}/{quote_sym} · {} · {tf_label} · . timeframe · f flip · {action}{}",
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
        Span::raw(" "),
        images::asset_span(app, t, &app.pool_icon_contract(quote), &quote_sym),
        Span::raw(" "),
        Span::styled(format!("{} {quote_sym}", price.map(fmt_price).unwrap_or_else(|| "—".into())), t.strong_style()),
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
    // A curve still selling has no pool to measure: it has what it raised toward graduation. A
    // bonded one trades against a pool its graduation seeded and locked, and that is its depth.
    let depth = match &pool.curve {
        Some(c) if let Some(locked) = c.locked_quai => {
            format!(" · locked {} QUAI · TVL {} · no LP token", fmt_qty(locked), locked_tvl(app, pool, c))
        }
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
        line1.push(Span::styled(format!("   {} alert {}", t.icon(Icon::Bell), set.join(" · ")), Style::default().fg(t.attention)));
    }
    let line2 = vec![
        Span::styled(format!("vol {} {quote_sym}{}", fmt_qty(stats.volume_24h), usd(stats.volume_24h)), t.text_style()),
        Span::styled(format!(" · {}", amount::count(stats.trades_24h, "trade")), t.dim_style()),
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
            empty_state(f, chart_area, t, t.icon(Icon::Danger), &e, &[("R", "retry")]);
        }
        Some(ev) => {
            let chart_w = chart_area.width.saturating_sub(11).max(8);
            let step = if chart_w as usize / 2 >= 24 { 2 } else { 1 };
            let n = ((chart_w / step) as usize).min(super::super::eco::MARKET_CANDLES);
            let _ = ev;
            // Dragged back in time, if the chart was; another pair or timeframe starts at now.
            let (pan, pan_pair, pan_tf) = &app.eco.markets_view.pan;
            let pan = if *pan_pair == pool.address && *pan_tf == app.eco.markets_view.timeframe { *pan } else { 0 };
            let cs = if pan == 0 {
                app.chart_candles(pool, base0, bucket, n)
            } else {
                app.chart_candles_at(pool, base0, bucket, n, wallet_core::registry::now().saturating_sub(pan as u64 * bucket))
            };
            if cs.is_empty() {
                empty(f, chart_area, t, Icon::Trade, "No trades in this window yet.", &[(".", "longer timeframe")]);
            } else {
                app.hits.borrow_mut().add(chart_area, crate::tui::hit::Target::Scroll(crate::tui::hit::Scroll::Chart));
                draw_candles(f, t, chart_area, &cs, step, bucket, app.pointer.at);
                draw_volume(f, t, volume_area, &cs, step);
            }
            draw_trade_tape(f, app, t, tape_area, &app.market_trades(pool, base0), &base_sym, &quote_sym);
        }
    }
}

/// A market's price before its history loads, base-per-quote as the list shows it: from the pool's
/// reserves, or a curve's own mark.
pub(crate) fn listed_price(pool: &wallet_core::markets::Pool, base0: bool) -> Option<f64> {
    pool.spot_price().map(|p| if base0 { p } else { 1.0 / p })
}

pub(crate) fn holding_line(
    app: &App,
    base: &wallet_core::markets::PoolToken,
    quote: &wallet_core::markets::PoolToken,
    bs: &str,
    qs: &str,
) -> String {
    let Some(p) = &app.eco.portfolio else { return String::new() };
    let wquai = app.net().and_then(|n| n.wquai.clone()).map(|w| w.to_lowercase());
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
pub(crate) fn draw_candles(
    f: &mut Frame,
    t: &Theme,
    area: Rect,
    cs: &[wallet_core::markets::Candle],
    step: u16,
    bucket: u64,
    pointer: Option<(u16, u16)>,
) {
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
        let style = Style::default().fg(if up { t.up } else { t.down });
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
                // Without color, a falling candle's body is shaded: up and down still differ.
                (true, true) if !up && t.monochrome => Some('▒'),
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
                cell.set_char(ch).set_style(t.strong_style());
            }
        }
    }
    // The pointer over the plot: a crosshair down the candle under it, and that candle in words
    // along the top (a readout, never over the axis figures).
    if let Some((px, py)) = pointer.filter(|(px, py)| *px >= plot.x && *px < plot.right() && *py >= plot.y && *py < plot.bottom()) {
        let i = ((px - plot.x) / step.max(1)) as usize;
        if let Some(c) = cs.get(i) {
            let cx = plot.x + i as u16 * step;
            for row in plot.top()..plot.bottom() {
                if let Some(cell) = buf.cell_mut((cx, row))
                    && matches!(cell.symbol(), " " | "┈")
                {
                    cell.set_char('┊').set_style(t.dim_style());
                }
            }
            let _ = py;
            let readout = format!(
                " {} · O {} H {} L {} C {} · {} ",
                local_time_label(c.start, bucket),
                fmt_price(c.open),
                fmt_price(c.high),
                fmt_price(c.low),
                fmt_price(c.close),
                wallet_core::amount::count(c.trades, "trade")
            );
            let w = (readout.chars().count() as u16).min(plot.width);
            let rx = if cx + w + 2 < plot.right() { cx + 2 } else { plot.x };
            for (k, ch) in readout.chars().take(w as usize).enumerate() {
                if let Some(cell) = buf.cell_mut((rx + k as u16, plot.y)) {
                    cell.set_char(ch).set_style(t.strong_style().bg(t.raised));
                }
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

pub(crate) fn draw_volume(f: &mut Frame, t: &Theme, area: Rect, cs: &[wallet_core::markets::Candle], step: u16) {
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
        let style = Style::default().fg(if c.close >= c.open { t.up } else { t.down });
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
pub(crate) fn draw_dex_flow(f: &mut Frame, app: &App, t: &Theme, area: Rect, pools: &[wallet_core::markets::Pool]) {
    let mv = &app.eco.markets_view;
    let focused = app.screen == Screen::Markets && app.lit_pane() == Some(1);
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
            (Some(e), _) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
            (None, true) => empty_state(f, inner, t, spinner(), "Watching for swaps…", &[]),
            (None, false) => empty(f, inner, t, Icon::Swap, "No swaps in the last few minutes.", &[]),
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
        return empty(f, inner, t, Icon::Swap, &format!("No swaps over {floor} in the tape."), &[(".", "lower the floor")]);
    }
    // The cursor only lives here while this pane has the focus.
    let cursor = (focused && rows_h > 0).then(|| app.selected.min(visible.len() - 1));
    let flow_id = crate::tui::hit::ListId::Screen(Screen::Markets, 1);
    let offset = app.pane_window(flow_id, visible.len(), rows_h);
    {
        let mut hits = app.hits.borrow_mut();
        let body = Rect { y: inner.y + u16::from(header), height: rows_h as u16, ..inner };
        hits.rows(flow_id, body, offset, visible.len(), |_| None);
    }
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
                Span::styled("you", t.strong_style())
            } else {
                Span::styled(flow_age(s.at), t.dim_style())
            };
            // Both logos, then the names once: the icon carries the symbol's letters where
            // bitmaps cannot be placed, exactly as the pairs list draws a market.
            let (from, to) = (app.market_symbol(&s.token_in), app.market_symbol(&s.token_out));
            let plain = if ours { t.strong_style() } else { t.text_style() };
            let strong = t.strong_style();
            let pair = Line::from(vec![
                // Your own trades carry a mark down the column, so they are findable at a glance.
                Span::styled(if ours { "▌" } else { " " }, t.strong_style()),
                images::asset_span(app, t, &app.pool_icon_contract(&s.token_in), &from),
                Span::raw(" "),
                images::asset_span(app, t, &app.pool_icon_contract(&s.token_out), &to),
                Span::raw(" "),
                Span::styled(truncate(&from, sym_w), plain),
                Span::styled("→", Style::default().fg(color)),
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

pub(crate) fn draw_trade_tape(
    f: &mut Frame,
    app: &App,
    t: &Theme,
    area: Rect,
    tr: &[wallet_core::markets::Trade],
    base: &str,
    quote: &str,
) {
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let rows: Vec<Row> = tr
        .iter()
        .take(area.height.saturating_sub(1) as usize)
        .map(|x| {
            let side =
                if x.buy { Span::styled("buy ", Style::default().fg(t.ok)) } else { Span::styled("sell", Style::default().fg(t.danger)) };
            let trader = if mine.contains(&x.trader.to_lowercase()) {
                Span::styled("you", t.strong_style())
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
