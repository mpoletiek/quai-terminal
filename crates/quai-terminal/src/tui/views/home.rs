//! Home: the portfolio hero, holdings, allocation and what needs attention.

use super::*;

// ---------------------------------------------------------------- Home

pub(crate) fn sparkline(values: &[f64], width: usize) -> String {
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
    // On a short terminal it is two lines, the total and what it takes in, so the holdings keep
    // their rows.
    // Sized text (OSC 66) is two rows where block digits are three, so its panel is a row shorter.
    let sizing = app.term.caps.text_sizing && app.config.big_numbers && !app.term.plain && !app.term.short;
    let hero_h = if app.term.short {
        4
    } else if sizing {
        6.min(area.height.saturating_sub(6)).max(3)
    } else {
        7.min(area.height.saturating_sub(6)).max(3)
    };
    let [top, bottom] = Layout::vertical([Constraint::Length(hero_h), Constraint::Min(4)]).areas(area);
    // The hero is read, not worked in: it never takes focus, so the holdings below are the one
    // lit panel on pane 0.
    let block = panel(t, "portfolio", false);
    let inner = block.inner(top);
    f.render_widget(block, top);
    match (app.eco.feeds.portfolio.value(), app.eco.feeds.portfolio.error()) {
        (Some(p), _) => {
            let pools = app.pools_usd();
            let total = amount::usd(p.total_usd + pools);
            // The `▌` beside the total lights when value arrives, and fades back to its color.
            let gutter = app
                .fx
                .gutter_flash
                .map(|s| s.elapsed().as_millis())
                .filter(|ms| *ms < super::super::edge::FLASH_MS)
                .and_then(|ms| super::super::edge::fade_to(t.ok, t.focus, ms as f32 / super::super::edge::FLASH_MS as f32))
                .unwrap_or(t.focus);
            let change = p.change_7d.map(|c| num::pct(c, 1)).unwrap_or_default();
            // Plain: the change is said as a number; a row of block glyphs is noise to a reader.
            let spark = if app.term.plain { String::new() } else { sparkline(&p.history.iter().map(|v| v.usd).collect::<Vec<_>>(), 16) };
            let whole = total.trim_start_matches('$').split('.').next().unwrap_or("0").to_string();
            let big = !sizing
                && app.config.big_numbers
                && !app.term.plain
                && !app.term.short
                && hero_fits(inner.width.saturating_sub(34), inner.height, &[&whole]);
            let mut y = inner.y;
            // Where the terminal can draw text larger than a cell (kitty's OSC 66), the total is
            // in the user's own font at twice the size, cents beside it at text size. Under a
            // modal or an effect it is one line of ordinary text, so the dimming covers it too
            // and nothing moves.
            let sized = sizing && matches!(app.modal, app::Modal::None) && app.fx.ambient.is_none();
            if sized {
                use super::super::term::backend::{BIG_TEXT_CELL, BigText};
                let (head, cents) = total.split_once('.').map_or((total.as_str(), String::new()), |(h, c)| (h, format!(".{c}")));
                let x = inner.x + 2;
                let buf = f.buffer_mut();
                // On the panel's own background, whatever the theme makes it.
                let bg = buf.cell((x, y)).map_or(t.surface, |c| c.bg);
                let text = BigText { x, y, scale: 2, text: head.to_string(), fg: t.strong, bg, bold: true };
                let w = text.width();
                for row in 0..2 {
                    if let Some(c) = buf.cell_mut((inner.x, y + row)) {
                        c.set_symbol("▌").set_fg(gutter);
                    }
                    for col in 0..w {
                        if let Some(c) = buf.cell_mut((x + col, y + row)) {
                            c.set_symbol(BIG_TEXT_CELL).set_style(t.strong_style());
                        }
                    }
                }
                f.render_widget(Paragraph::new(Span::styled(cents, t.dim_style())), Rect { x: x + w, y: y + 1, width: 6, height: 1 });
                app.term.big_text.borrow_mut().push(text);
                y += 2;
            } else if big {
                let rows = big_digits(&whole);
                let frac = total.split_once('.').map(|(_, f)| format!(".{f}")).unwrap_or_default();
                for (i, row) in rows.iter().enumerate() {
                    let mut spans = vec![
                        Span::styled(if i == 0 { "▌$ " } else { "▌  " }, Style::default().fg(gutter)),
                        Span::styled(row.clone(), t.strong_style()),
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
                        Span::styled("▌ ", Style::default().fg(gutter)),
                        Span::styled(total.clone(), t.strong_style()),
                    ])),
                    Rect { y, height: 1, ..inner },
                );
                y += 1;
            }
            let trend = if p.change_7d.unwrap_or(0.0) >= 0.0 { t.up } else { t.down };
            let mut right = super::super::edge::spark_spans(app, t, &spark, trend);
            right.push(Span::raw(" "));
            let right = Line::from(
                right
                    .into_iter()
                    .chain([
                        Span::styled(if p.history.is_empty() { String::new() } else { "7d  ".into() }, t.dim_style()),
                        Span::styled(change, Style::default().fg(if p.change_7d.unwrap_or(0.0) >= 0.0 { t.up } else { t.down })),
                    ])
                    .collect::<Vec<_>>(),
            );
            f.render_widget(Paragraph::new(right).alignment(Alignment::Right), Rect { y: inner.y, height: 1, ..inner });
            // Beneath the change: where the figures came from, and what the total takes in.
            let mut notes = Vec::new();
            if p.stale {
                notes.push(format!("{} some prices from cache", t.icon(Icon::Stale)));
            }
            if pools > 0.0 {
                notes.push(format!("incl. {} in pools", amount::usd(pools)));
            }
            if app.term.short {
                notes = vec![notes.join(" · ")];
            }
            for (k, note) in notes.into_iter().enumerate() {
                if inner.y + 1 + (k as u16) < inner.bottom() {
                    f.render_widget(
                        Paragraph::new(Span::styled(note, t.dim_style())).alignment(Alignment::Right),
                        Rect { y: inner.y + 1 + k as u16, height: 1, ..inner },
                    );
                }
            }
            // NFT thumbnails (reference only, never in the total) on the right when there is room.
            let thumbs: Vec<(String, String, String, String)> = match app.eco.nft.nfts.latest() {
                Some(Ok(v)) if app.config.features.nfts && app.config.images && !app.term.plain => v
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
                if !app.term.plain {
                    more.push(Span::styled("◧ ", Style::default().fg(t.qi)));
                }
                let counted = if app.term.short {
                    format!("{}  ", amount::count(p.nfts.items, "NFT"))
                } else {
                    format!(
                        "{} in {} · not counted in the total  ",
                        amount::count(p.nfts.items, "NFT"),
                        amount::count(p.nfts.collections, "collection")
                    )
                };
                more.push(Span::styled(counted, t.dim_style()));
                more.push(Span::styled(Section::Nfts.key().to_string(), Style::default().fg(t.focus)));
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
        (None, Some(e)) => empty_state(
            f,
            inner,
            t,
            t.icon(Icon::Danger),
            &format!("Portfolio unavailable: {}", app::friendly_error(e)),
            &[("g d", "data sources")],
        ),
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
    draw_holdings(f, app, t, holdings, app.lit_pane() == Some(0));
    let mut items: Vec<Line> = Vec::new();
    let op_unlocks: Vec<u64> = app
        .dash
        .ops
        .iter()
        .filter(|o| o.status == wallet_core::appdb::OpStatus::Locked)
        .filter_map(|o| o.detail.unlock_height().as_u64())
        .collect();
    for news in &app.news.unlocked_news {
        items.push(Line::from(vec![Span::styled(t.lead(Icon::Ok), Style::default().fg(t.ok)), Span::raw(news.clone())]));
    }
    for l in app.dash.locks.iter().filter(|l| !l.unlocked && !l.unlock_height.is_some_and(|h| op_unlocks.contains(&h))).take(3) {
        let eta = l.eta_secs.map(|s| format!(" in {}", wallet_core::track::human_duration(s))).unwrap_or_default();
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::Locked), Style::default().fg(t.pending)),
            Span::raw(format!("{} {} unlocks{eta}", l.amount, num::unit(&l.asset))),
        ]));
    }
    if let Some(qits) = app.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.as_deref()).and_then(|q| q.parse::<U256>().ok())
        && !qits.is_zero()
    {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::Attention), Style::default().fg(t.attention)),
            Span::raw(format!("{} Qi wrapped, ready to claim as WQI ({})", num::qi(qits), Screen::Wrap.place())),
        ]));
    }
    if let Some(plan) = &app.eco.plan {
        use quai_engine::plans::Phase;
        let state = match &plan.view.phase {
            Phase::Waiting(_) if plan.view.last == Some(wallet_core::journal::OpKind::Approve) => "waiting for the approval to confirm",
            Phase::Waiting(_) => "waiting for the last step to confirm",
            Phase::Reviewing(_) => "review open",
            _ if plan.requested => "review open",
            _ => "preparing the next step",
        };
        let mut line =
            vec![Span::styled(t.lead(Icon::InFlight), Style::default().fg(t.pending)), Span::raw(format!("{} · ", plan.view.label))];
        line.extend(super::super::widgets::stepper(t, &plan.view.stepper(None)));
        line.push(Span::styled(format!(" · {state}"), t.dim_style()));
        items.push(Line::from(line));
    }
    // Unfinished operations, by what they wait on; only unmined ones can use the user.
    use wallet_core::appdb::OpStatus;
    let head = app.dash.health.as_ref().map(|h| h.height).unwrap_or(0);
    let open: Vec<&wallet_core::appdb::Operation> = app.dash.ops.iter().filter(|o| !o.status.is_terminal()).collect();
    let confirming = open.iter().filter(|o| !matches!(o.status, OpStatus::Settling | OpStatus::Locked)).count();
    if confirming > 0 {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::InFlight), Style::default().fg(t.pending)),
            Span::raw(format!("{} waiting to be mined · ", amount::count(confirming, "transaction"))),
        ]));
        items.last_mut().expect("just pushed").spans.extend(place_spans(t, Screen::Activity));
    }
    let settling = open.iter().filter(|o| o.status == OpStatus::Settling).count();
    if settling > 0 {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::InFlight), Style::default().fg(t.pending)),
            Span::raw(format!("{settling} settling on the destination chain · nothing to do")),
        ]));
    }
    for o in open.iter().filter(|o| o.status == OpStatus::Locked).take(2) {
        let eta = o
            .detail
            .unlock_height()
            .as_u64()
            .filter(|u| *u > head && head > 0)
            .map(|u| format!(" in ~{}", wallet_core::track::human_duration((u - head) * 5)))
            .unwrap_or_default();
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::Locked), Style::default().fg(t.pending)),
            Span::raw(format!("{} unlocks{eta} · automatic, nothing to do", truncate(&describe(o), 30))),
        ]));
    }
    // Messages waiting in the channels this wallet follows, from wherever you are.
    let waiting: u32 = app.config.board_channels.iter().map(|c| app.board_unread(c)).sum();
    if waiting > 0 {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::On), Style::default().fg(t.link)),
            Span::raw(format!("{} on the board · ", amount::count(waiting, "new message"))),
        ]));
        items.last_mut().expect("just pushed").spans.extend(place_spans(t, Screen::Board));
    }
    let unread = app.dash.notifications.iter().filter(|n| !n.read).count();
    if unread > 0 {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::Bell), Style::default().fg(t.attention)),
            Span::raw(format!("{} · ", amount::count(unread, "unread notification"))),
            Span::styled("N", t.strong_style().fg(t.focus)),
            Span::styled(" to read", t.dim_style()),
        ]));
    }
    if app.meta.as_ref().is_some_and(|m| m.kind == wallet_core::registry::WalletKind::Hd && !m.backed_up) {
        items.push(Line::from(vec![
            Span::styled(t.lead(Icon::Attention), Style::default().fg(t.attention)),
            Span::raw("recovery phrase not verified"),
        ]));
    }
    if let Some(p) = app.eco.feeds.portfolio.value() {
        if p.stale {
            items.push(Line::from(vec![
                Span::styled(t.lead(Icon::Stale), t.dim_style()),
                Span::raw("some data is stale (source did not answer)"),
            ]));
        }
        for n in p.notices.iter().take(2) {
            items.push(Line::from(vec![Span::styled(t.lead(Icon::Danger), Style::default().fg(t.danger)), Span::raw(truncate(n, 60))]));
        }
        if p.unpriced > 0 {
            items.push(Line::from(vec![
                Span::styled(t.lead(Icon::Info), t.dim_style()),
                Span::styled(format!("{} without a price", amount::count(p.unpriced, "holding")), t.dim_style()),
            ]));
        }
    }
    if app.dash.node_error.is_some() {
        items.push(Line::from(vec![Span::styled(t.lead(Icon::Danger), Style::default().fg(t.danger)), Span::raw("node unreachable")]));
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
    let title = if app.news.first_payment.is_some() {
        // Once in a wallet's life, on chrome: the first payment it ever received.
        format!("recent activity · your first payment {}", t.icon(Icon::Ok))
    } else if items.is_empty() {
        format!("recent activity · {} all clear", t.icon(Icon::Ok))
    } else {
        "recent activity".to_string()
    };
    let block = panel(t, &title, app.lit_pane() == Some(1));
    let inner = block.inner(recent);
    f.render_widget(block, recent);
    // when (4) · arrow (1) · description · status mark (1), with a cell between each.
    let text_w = inner.width.saturating_sub(4 + 1 + 1 + 3) as usize;
    let mut rows = activity_table_rows(app, t, inner.height as usize, false, Some(text_w));
    {
        let mut hits = app.input.hits.borrow_mut();
        hits.add(recent, crate::tui::hit::Target::Pane(1));
        let all = app.activity_rows();
        let n = rows.len();
        hits.rows(crate::tui::hit::ListId::Screen(app.nav.screen, 1), inner, 0, n, |i| all.get(i).map(|r| app.activity_row_key(r)));
    }
    if rows.is_empty() {
        empty(
            f,
            inner,
            t,
            Icon::Activity,
            "Quiet chain, quiet mind. Sends, receipts, swaps and NFT moves appear here as they happen.",
            &[("r", "receive"), ("t", "trade")],
        );
        return;
    }
    if app.lit_pane() == Some(1) {
        rows = rows.into_iter().enumerate().map(|(i, r)| if i == app.nav.selected { r.style(t.selected()) } else { r }).collect();
    }
    f.render_widget(
        Table::new(rows, [Constraint::Length(4), Constraint::Length(1), Constraint::Min(10), Constraint::Length(1)]).column_spacing(1),
        inner,
    );
}

// ---------------------------------------------------------------- Home › holdings

/// The holdings table. Lives on Home, which is the only place it is drawn.
pub fn draw_holdings(f: &mut Frame, app: &App, t: &Theme, area: Rect, focused: bool) {
    let Some(p) = app.eco.feeds.portfolio.value() else {
        let block = panel(t, "holdings", focused);
        let inner = block.inner(area);
        f.render_widget(block, area);
        match app.eco.feeds.portfolio.error() {
            Some(e) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "refresh")]),
            None => empty_state(f, inner, t, spinner(), "Pricing your holdings…", &[]),
        }
        return;
    };
    // The legend explains the marks; it is there when asked for (`i`), not holding a third of the
    // screen for someone who read it once.
    let wide = area.width >= 110 && app.eco.info_open;
    let [list, side] =
        if wide { Layout::horizontal([Constraint::Min(70), Constraint::Length(32)]).areas(area) } else { [area, Rect::default()] };
    // Liquidity positions are holdings too: listed after the tokens, and in the total.
    let lps = app.home_positions();
    let count = p.rows.len() + lps.len();
    let block = panel(t, &format!("holdings · {} · {}", count, amount::usd(p.total_usd + app.pools_usd())), focused);
    let mut inner = block.inner(list);
    f.render_widget(block, list);
    // Wrapped Qi is not WQI until it is claimed; say so where people look for it.
    if let Some(qits) = app.dash.wrap.as_ref().and_then(|w| w.unclaimed_qits.as_deref()).and_then(|q| q.parse::<U256>().ok())
        && !qits.is_zero()
        && inner.height > 3
    {
        f.render_widget(
            Paragraph::new(Line::from(
                vec![
                    Span::styled(t.lead(Icon::Attention), Style::default().fg(t.attention)),
                    Span::raw(format!("{} Qi wrapped and waiting to be claimed as WQI · ", num::qi(qits))),
                ]
                .into_iter()
                .chain(place_spans(t, Screen::Wrap))
                .collect::<Vec<_>>(),
            )),
            Rect { height: 1, ..inner },
        );
        inner = Rect { y: inner.y + 1, height: inner.height - 1, ..inner };
    }
    let narrow = inner.width < 84;
    // Headers sit over their values: numbers are right-aligned, so their headers are too.
    let right = |h: &'static str| Cell::from(Line::from(h).alignment(Alignment::Right));
    let header: Vec<Cell> = if narrow {
        vec![Cell::from(""), Cell::from("asset"), right("balance"), right("value"), Cell::from("src")]
    } else {
        vec![
            Cell::from(""),
            Cell::from("asset"),
            right("balance"),
            right("price"),
            right("value"),
            Cell::from("alloc"),
            right("24h"),
            Cell::from("src"),
        ]
    };
    let icon_col: Vec<(Rect, &AssetRow)> = Vec::new();
    let mut icons = icon_col;
    let visible = inner.height.saturating_sub(1) as usize;
    let list_id = crate::tui::hit::ListId::Screen(app.nav.screen, 0);
    let offset = app.pane_window(list_id, count, visible);
    {
        let mut hits = app.input.hits.borrow_mut();
        hits.add(list, crate::tui::hit::Target::Pane(0));
        let body = Rect { y: inner.y + 1, height: visible as u16, ..inner };
        hits.rows(list_id, body, offset, count, |i| {
            p.rows.get(i).map(|r| r.key.id()).or_else(|| lps.get(i - p.rows.len()).map(|lp| format!("lp:{}", lp.pair)))
        });
    }
    let balances = num::align(&p.rows.iter().skip(offset).take(visible).map(balance_text).collect::<Vec<_>>());
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
                Cell::from(Line::from(balances[i - offset].clone()).alignment(Alignment::Right)),
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
                cells.push(Cell::from(
                    Line::from(match r.change_24h {
                        Some(c) => Span::styled(num::pct(c, 1), Style::default().fg(if c >= 0.0 { t.up } else { t.down })),
                        None => Span::styled("—", t.dim_style()),
                    })
                    .alignment(Alignment::Right),
                ));
            }
            cells.push(Cell::from(src_span(t, r, p.stale)));
            let row = Row::new(cells);
            if i == app.nav.selected { row.style(t.selected()) } else { row }
        })
        .collect();
    // Positions after the tokens: the pair, the share of the pool held, and its value.
    let mut rows = rows;
    let shown = rows.len();
    for (k, lp) in lps.iter().enumerate().skip(offset.saturating_sub(p.rows.len())).take(visible.saturating_sub(shown)) {
        let i = p.rows.len() + k;
        let held = lp.lp_wallet.saturating_add(lp.lp_staked);
        let share = wallet_core::amount::ratio(held, lp.lp_total).map_or_else(|| "—".to_string(), |r| format!("{:.2}% of pool", r * 100.0));
        let dash = || Cell::from(Line::from(Span::styled("—", t.dim_style())).alignment(Alignment::Right));
        let mut cells = vec![
            Cell::from("  "),
            Cell::from(Line::from(vec![
                Span::styled(t.lead(Icon::Pool), Style::default().fg(t.link)),
                Span::styled(truncate(&format!("{}/{}", lp.token0.symbol, lp.token1.symbol), 11), t.strong_style()),
            ])),
            Cell::from(Line::from(Span::styled(share, t.text_style())).alignment(Alignment::Right)),
        ];
        if !narrow {
            cells.push(dash());
        }
        cells.push(Cell::from(Line::from(lp.usd.map(amount::usd).unwrap_or_else(|| "—".into())).alignment(Alignment::Right)));
        if !narrow {
            cells.push(Cell::from(Span::styled("—", t.dim_style())));
            cells.push(dash());
        }
        cells.push(Cell::from(Span::styled(if lp.lp_staked.is_zero() { "pool" } else { "staked" }, t.dim_style())));
        let row = Row::new(cells);
        rows.push(if i == app.nav.selected { row.style(t.selected()) } else { row });
    }
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
    let used = 1 + count.min(visible) as u16;
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
    lines.push(Line::from(vec![Span::styled(t.lead(Icon::Ok), Style::default().fg(t.ok)), Span::raw("verified contract")]));
    lines.push(Line::from(vec![
        Span::styled(t.lead(Icon::Warning), Style::default().fg(t.attention)),
        Span::raw("unverified — never auto-trusted"),
    ]));
    lines.push(Line::from(vec![Span::styled(t.lead(Icon::On), Style::default().fg(t.ok)), Span::raw("market price")]));
    lines.push(Line::from(vec![Span::styled(t.lead(Icon::Protocol), Style::default().fg(t.qi)), Span::raw("protocol-derived (Qi)")]));
    lines.push(Line::from(vec![Span::styled(t.lead(Icon::Stale), t.dim_style()), Span::raw("stale, with age")]));
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
        lines.push(Line::from(Span::styled(format!("◧ {} · not in total", amount::count(p.nfts.items, "NFT")), t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }).block(panel(t, "about prices", false)), side);
}

/// The portfolio's allocation: a stacked bar across the width, then the largest holdings named
/// with their share. Anything under half a percent is folded into "other".
pub(crate) fn draw_allocation(f: &mut Frame, app: &App, t: &Theme, area: Rect, p: &wallet_core::portfolio::Portfolio) {
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
    // Each part its own colour: an asset's tint where it has one, and a categorical chart colour
    // for the ones that would otherwise share the dim grey of an unverified token. (Never a state
    // colour: a holding is not an error.)
    let spare = t.chart;
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
        draw_value_chart(f, t, Rect { y: chart_y, height: area.bottom() - chart_y, ..area }, &p.history, p.change_7d, chart_marker(app));
    }
}

/// The portfolio's value over the history the portfolio carries (seven days), as a line with its
/// range on the axis. Honest about what it is: QUAI's balance history at today's price, other
/// holdings held constant — the hero's caption says so, and so does this one.
/// The finest marker a line chart can use here: octants (2×4 solid pixels a cell) where the
/// terminal draws them itself, braille dots (the same grid, in every Nerd Font) elsewhere.
pub(crate) fn chart_marker(app: &App) -> ratatui::symbols::Marker {
    if app.term.caps.drawn_blocks && !app.term.plain { ratatui::symbols::Marker::Octant } else { ratatui::symbols::Marker::Braille }
}

pub(crate) fn draw_value_chart(
    f: &mut Frame,
    t: &Theme,
    area: Rect,
    history: &[wallet_core::portfolio::ValuePoint],
    change: Option<f64>,
    marker: ratatui::symbols::Marker,
) {
    let points: Vec<(f64, f64)> = history.iter().map(|v| (v.at as f64, v.usd)).collect();
    let (x0, x1) = (points.first().map_or(0.0, |p| p.0), points.last().map_or(1.0, |p| p.0));
    let (lo, hi) = points.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p.1), b.max(p.1)));
    let pad = ((hi - lo) * 0.1).max(hi.abs() * 0.01).max(0.01);
    let (lo, hi) = ((lo - pad).max(0.0), hi + pad);
    let up = change.unwrap_or(0.0) >= 0.0;
    let colour = if up { t.up } else { t.down };
    let title = Line::from(vec![
        Span::styled("value · 7 days ", t.dim_style()),
        Span::styled(change.map(|c| num::pct(c, 1)).unwrap_or_default(), Style::default().fg(colour)),
        Span::styled("  QUAI balance history at today's price", t.dim_style()),
    ]);
    f.render_widget(Paragraph::new(title), Rect { height: 1, ..area });
    let chart_area = Rect { y: area.y + 1, height: area.height - 1, ..area };
    let dataset = Dataset::default().graph_type(GraphType::Line).marker(marker).style(Style::default().fg(colour)).data(&points);
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

pub(crate) fn draw_asset_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, id: &str) {
    let row = app.eco.feeds.portfolio.value().and_then(|p| p.rows.iter().find(|r| r.key.id() == id));
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
        lines.push(kv(t, "24h (cap)", Span::styled(num::pct(c, 2), Style::default().fg(if c >= 0.0 { t.up } else { t.down }))));
    }
    lines.push(kv(t, "allocation", Span::raw(format!("{:.1}%", r.allocation * 100.0))));
    if let AssetKey::Token(address) = &r.key {
        lines.push(kv(t, "contract", Span::styled(address.clone(), Style::default().fg(t.link))));
        match app.eco.feeds.token_info.get(address) {
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
        && let Some(p) = app.eco.feeds.portfolio.value()
        && !p.history.is_empty()
    {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![Span::styled("7d value ", t.dim_style())]));
        let spark = sparkline(&p.history.iter().map(|v| v.usd).collect::<Vec<_>>(), inner.width.saturating_sub(12) as usize);
        if let Some(last) = lines.last_mut() {
            last.spans.extend(super::super::edge::spark_spans(app, t, &spark, t.quai));
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
        Some(addr) => a.detail.token().as_str().is_some_and(|x| x.eq_ignore_ascii_case(addr)),
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
        .filter(|o| o.asset.eq_ignore_ascii_case(&r.symbol) || o.detail.token().as_str().is_some_and(|x| Some(x) == token))
    {
        lines.push(Line::from(vec![Span::styled(format!("{:<6} ", ago(o.created)), t.dim_style()), Span::raw(truncate(&describe(o), 34))]));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled("no transfers seen yet", t.dim_style())));
    }
    lines.truncate(rinner.height as usize);
    f.render_widget(Paragraph::new(lines), rinner);
}
