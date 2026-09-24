//! Trade › Pools: liquidity positions, the pool list and adding liquidity.

use super::*;

// ---------------------------------------------------------------- Trade › Pools

/// What you provide on the left, every pool on the right — because a new position starts from the
/// directory, not from something you already hold.
pub fn draw_pools(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::gauge::apr_text;
    let title = "pools · Quainance";
    if app.net().is_some_and(|n| n.ecosystem.quainance_router.is_none()) {
        let block = panel(t, title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty(f, inner, t, Icon::Pool, "No Quainance pools on this network (mainnet only).", &[("[", "swap")]);
    }
    if let Some(Err(e)) = &app.eco.pools_view.positions {
        let block = panel(t, title, true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]);
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
pub(crate) fn draw_add_liquidity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use super::super::images::asset_span;
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
        value
    };
    let account = card.account.as_ref().and_then(|a| app.dash.accounts.iter().find(|x| &x.address == a)).map(|a| a.label.clone());
    let mut c = Card::new();
    c.field(
        t,
        0,
        card.field,
        "account",
        match account {
            Some(a) => cycler(t, a, card.field == 0),
            None => vec![Span::styled("—", t.dim_style())],
        },
    );
    c.line(Line::from(""));
    c.line(Line::from(Span::styled("you deposit", t.dim_style())));
    c.field(t, 1, card.field, &card.token0.symbol.to_lowercase(), side_row(1, &card.token0));
    c.field(t, 2, card.field, &card.token1.symbol.to_lowercase(), side_row(2, &card.token1));
    c.line(Line::from(Span::styled(
        format!("        type either side · m max · the pool sets the {}", card.paired().symbol),
        t.dim_style(),
    )));
    c.line(Line::from(""));
    c.field(
        t,
        3,
        card.field,
        "slippage",
        vec![Span::styled(format!("{:.2}%  (type to change)", f64::from(card.slippage_bps) / 100.0), t.text_style())],
    );
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
        c.line(Line::from(""));
        c.line(step_line(t, &steps.iter().map(String::as_str).collect::<Vec<_>>(), 0));
        c.action(Line::from(Span::styled("enter · review the deposit", t.strong_style().fg(t.focus))), crossterm::event::KeyCode::Enter);
    }
    let block = panel(t, &format!("add liquidity · {}", card.name), true);
    c.hits(app, block.inner(left));
    f.render_widget(Paragraph::new(c.lines).block(block), left);

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
                    num::short(q.amount0_min, q.token0.decimals, 4),
                    q.token0.symbol,
                    num::short(q.amount1_min, q.token1.decimals, 4),
                    q.token1.symbol
                )),
            ));
            // What is already in place, so nobody pays a fee to approve something twice.
            let approval = |needed: bool, token: &wallet_core::markets::PoolToken| {
                if needed {
                    Span::styled(format!("{} needed", token.symbol), Style::default().fg(t.attention))
                } else {
                    Span::styled(format!("{} {} approved", token.symbol, t.icon(Icon::Ok)), Style::default().fg(t.ok))
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
        Some(Err(e)) => q_lines
            .push(Line::from(Span::styled(format!("{} {}", t.icon(Icon::Danger), app::friendly_error(e)), Style::default().fg(t.danger)))),
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
pub(crate) fn draw_positions_pane(
    f: &mut Frame,
    app: &App,
    t: &Theme,
    area: Rect,
    positions: &[wallet_core::liquidity::LpPosition],
    now: u64,
) {
    use wallet_core::gauge::apr_text;
    let directory = app.directory_rows();
    let total: f64 = positions.iter().filter_map(|p| p.usd).sum();
    let heading = if positions.is_empty() { "your liquidity".to_string() } else { format!("your liquidity · {}", amount::usd(total)) };
    let block = panel(t, &heading, app.lit_pane() == Some(0));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.pools_view.positions.is_none() {
        return empty_state(f, inner, t, spinner(), "Reading your liquidity…", &[]);
    }
    if positions.is_empty() {
        return empty(
            f,
            inner,
            t,
            Icon::Pool,
            "You provide no liquidity yet. Pick a pool on the right and press a — you earn a share of every swap fee, and some pools pay gauge rewards on top.",
            &[("tab", "pool list"), ("a", "add liquidity")],
        );
    }
    let selected = app.eco.pools_view.selected.min(positions.len() - 1);
    let mut lines = Vec::new();
    // Each position takes two lines or more (its rewards spelled out when focused): a click
    // anywhere on its lines selects it.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for (i, p) in positions.iter().enumerate() {
        spans.push((i, lines.len()));
        let gauge = app.gauge_pool_for(&p.pair);
        let zone = if gauge.is_none() { app.zone_pool_for(&p.pair) } else { None };
        let apr = app.pool_apr(p);
        let pending = gauge.is_some_and(wallet_core::gauge::GaugePool::has_rewards) || zone.is_some_and(|z| z.has_rewards());
        let focused = i == selected && app.lit_pane() == Some(0);
        let style = if focused { t.selected() } else { t.text_style() };
        let mark = match (gauge.is_some() || zone.is_some(), !p.lp_staked.is_zero()) {
            (true, true) => Span::styled(format!("{} staked", t.icon(Icon::Staked)), Style::default().fg(t.ok)),
            (true, false) => Span::styled(format!("{} stakeable", t.icon(Icon::Off)), Style::default().fg(t.attention)),
            (false, _) => Span::raw(""),
        };
        let mut row = vec![Span::styled(if focused { "▌" } else { " " }, Style::default().fg(t.focus))];
        // Named as the pool list and Markets name its pool, when the directory carries it.
        let (name, icons) = match directory.iter().find(|d| d.address.eq_ignore_ascii_case(&p.pair)) {
            Some(d) => {
                let (base, quote) = if app.pool_base0(d) { (&d.token0, &d.token1) } else { (&d.token1, &d.token0) };
                (app.pair_name(d), pair_icons(app, t, base, quote))
            }
            None => (p.name(), pair_icons(app, t, &p.token0, &p.token1)),
        };
        row.extend(icons);
        row.extend([
            Span::styled(format!("{:<14}", truncate(&name, 14)), style.add_modifier(Modifier::BOLD)),
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
            Span::styled(if pending { format!("  {} rewards", t.icon(Icon::On)) } else { String::new() }, Style::default().fg(t.attention)),
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
                lines.push(Line::from(Span::styled("      space s stakes it and starts earning", Style::default().fg(t.attention))));
            }
        }
        // A launch-zone campaign on this pair, which the core gauge knows nothing about.
        if focused && let Some(z) = zone {
            lines.extend(zone_campaign_lines(app, t, z, now, app.pool_tvl(&p.pair)));
            if p.lp_staked.is_zero() && !p.lp_wallet.is_zero() {
                lines
                    .push(Line::from(Span::styled("        space s stakes it in the launch-zone gauge", Style::default().fg(t.attention))));
            }
        }
    }
    {
        let mut hits = app.hits.borrow_mut();
        hits.add(area, crate::tui::hit::Target::Pane(0));
        let ends: Vec<usize> = spans.iter().skip(1).map(|(_, s)| *s).chain(std::iter::once(lines.len())).collect();
        for ((i, start), end) in spans.iter().zip(ends) {
            let (top, bottom) = (*start as u16, (end as u16).min(inner.height));
            if top < bottom {
                let rect = Rect { y: inner.y + top, height: bottom - top, ..inner };
                hits.add(
                    rect,
                    crate::tui::hit::Target::Row { list: crate::tui::hit::ListId::Screen(Screen::Pools, 0), index: *i, key: None },
                );
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
pub(crate) fn zone_campaign_lines(
    app: &App,
    t: &Theme,
    z: &wallet_core::zone::ZonePool,
    now: u64,
    tvl_usd: Option<f64>,
) -> Vec<Line<'static>> {
    use wallet_core::gauge::apr_text;
    use wallet_core::zone::Genesis;
    let state = z.campaign.state(now);
    let colour = match state {
        Genesis::Live => t.ok,
        Genesis::AwaitingActivation => t.attention,
        _ => t.dim,
    };
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("      {} launch-zone campaign · ", t.icon(Icon::Launch)), Style::default().fg(t.attention)),
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
        lines.push(Line::from(Span::styled(format!("        {} LP staked here", num::short(z.staked, 18, 6)), t.dim_style())));
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
pub(crate) fn draw_directory_pane(
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
    let block = panel(t, &title, app.lit_pane() == Some(1));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if pools.is_empty() {
        return empty_state(f, inner, t, spinner(), "Loading pools…", &[]);
    }
    let selected = app.eco.pools_view.pool_selected.min(pools.len() - 1);
    let height = inner.height.saturating_sub(1) as usize;
    let list_id = crate::tui::hit::ListId::Screen(Screen::Pools, 1);
    let start = app.list_window(list_id, selected, pools.len(), height);
    {
        let mut hits = app.hits.borrow_mut();
        hits.add(area, crate::tui::hit::Target::Pane(1));
        hits.rows(list_id, Rect { y: inner.y + 1, height: height as u16, ..inner }, start, pools.len(), |_| None);
    }
    let mut lines = vec![Line::from(vec![
        Span::styled(format!("{:<6}{:<16}", "", "pair"), t.dim_style()),
        Span::styled(format!("{:>9}  ", "TVL"), t.dim_style()),
        Span::styled(format!("{:>6}", "apr"), t.dim_style()),
    ])];
    for (i, pool) in pools.iter().enumerate().skip(start).take(height) {
        let focused = i == selected && app.lit_pane() == Some(1);
        let style = if focused { t.selected() } else { t.text_style() };
        // Named as Markets names it (base first, QUAI for wrapped QUAI), so one pool reads the
        // same on both tabs.
        let name = app.pair_name(pool);
        let (base, quote) = if app.pool_base0(pool) { (&pool.token0, &pool.token1) } else { (&pool.token1, &pool.token0) };
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
        row.extend(pair_icons(app, t, base, quote));
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
            Span::styled(if zone.is_some() { format!(" {}", t.icon(Icon::Launch)) } else { "  ".into() }, Style::default().fg(t.attention)),
            Span::styled(if held { t.icon(Icon::Staked) } else { "" }, Style::default().fg(t.ok)),
        ]);
        lines.push(Line::from(row));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// The two tokens of a pair as inline icons, six cells with the gap after, whatever the terminal
/// can draw:
/// bitmaps where kitty allows them, tinted monograms otherwise.
pub(crate) fn pair_icons(
    app: &App,
    t: &Theme,
    token0: &wallet_core::markets::PoolToken,
    token1: &wallet_core::markets::PoolToken,
) -> Vec<Span<'static>> {
    use super::super::images::asset_span;
    // Wrapped QUAI wears the QUAI logo: the pair reads as what it trades, not as its plumbing.
    let (c0, c1) = (app.pool_icon_contract(token0), app.pool_icon_contract(token1));
    vec![asset_span(app, t, &c0, &token0.symbol), Span::raw(" "), asset_span(app, t, &c1, &token1.symbol), Span::raw(" ")]
}
