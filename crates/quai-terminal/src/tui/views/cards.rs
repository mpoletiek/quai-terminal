//! The exchange cards (Swap, Convert, Wrap) and the trade views beside them: launches, PnL.

use super::*;

// ---------------------------------------------------------------- exchange cards

/// A card's lines, and which of them are fields and buttons, so the pointer can reach them.
pub(crate) struct Card<'a> {
    pub(crate) lines: Vec<Line<'a>>,
    /// (line, what a click there does)
    targets: Vec<(usize, crate::tui::hit::Target)>,
}

impl<'a> Card<'a> {
    pub(crate) fn new() -> Self {
        Card { lines: Vec::new(), targets: Vec::new() }
    }

    pub(crate) fn line(&mut self, line: Line<'a>) {
        self.lines.push(line);
    }

    /// Field `n` of the card (`current` is the focused one): a click focuses it.
    pub(crate) fn field(&mut self, t: &Theme, n: usize, current: usize, label: &str, value: Vec<Span<'a>>) {
        self.targets.push((self.lines.len(), crate::tui::hit::Target::CardField(n)));
        self.lines.push(card_row(t, n == current, label, value));
    }

    /// A line that is a key to press (the primary action): a click presses it.
    pub(crate) fn action(&mut self, line: Line<'a>, key: crossterm::event::KeyCode) {
        self.targets.push((self.lines.len(), crate::tui::hit::Target::Key(key)));
        self.lines.push(line);
    }

    /// Register the clickable lines, drawn unwrapped from the top of `inner`.
    pub(crate) fn hits(&self, app: &App, inner: Rect) {
        let mut hits = app.hits.borrow_mut();
        for (line, target) in &self.targets {
            let y = inner.y + *line as u16;
            if y < inner.bottom() {
                hits.add(Rect::new(inner.x, y, inner.width, 1), target.clone());
            }
        }
    }
}

pub(crate) fn card_row<'a>(t: &Theme, focused: bool, label: &str, value: Vec<Span<'a>>) -> Line<'a> {
    let mut spans = vec![
        Span::styled(if focused { "▌ " } else { "  " }, Style::default().fg(t.focus)),
        Span::styled(format!("{label:<12}"), t.dim_style()),
    ];
    spans.extend(value);
    Line::from(spans)
}

/// Where something is, said the way the header says it, with the chord that goes there as the
/// key: `Board g b`.
pub(crate) fn place_spans(t: &Theme, screen: Screen) -> Vec<Span<'static>> {
    let section = screen.section();
    let name =
        if section.all_screens().len() == 1 { section.title().to_string() } else { format!("{} › {}", section.title(), screen.title()) };
    vec![Span::styled(format!("{name} "), t.text_style()), Span::styled(super::super::keymap::chord(screen), t.strong_style().fg(t.focus))]
}

/// A value ←/→ steps through: the value, then `‹›`, lit while its row has focus.
pub(crate) fn cycler(t: &Theme, value: String, focused: bool) -> Vec<Span<'static>> {
    vec![Span::styled(value, t.strong_style()), Span::styled(" ‹›", if focused { Style::default().fg(t.focus) } else { t.dim_style() })]
}

/// A settings-style value: on (`●`) with the mark in `ok`, off (`○`) dim, anything else as text.
pub(crate) fn value_line(t: &Theme, value: String) -> Line<'static> {
    let (on, off) = (t.lead(Icon::On), t.lead(Icon::Off));
    if let Some(rest) = value.strip_prefix(on.as_str()) {
        Line::from(vec![Span::styled(on.clone(), Style::default().fg(t.ok)), Span::styled(rest.to_string(), t.text_style())])
    } else if value.starts_with(off.as_str()) {
        Line::from(Span::styled(value, t.dim_style()))
    } else {
        Line::from(Span::styled(value, t.text_style()))
    }
}

pub(crate) fn amount_span(t: &Theme, text: &str, focused: bool) -> Span<'static> {
    let shown = if text.is_empty() { "0".to_string() } else { num::typed(text) };
    if focused {
        Span::styled(format!("{shown}▏"), t.strong_style())
    } else {
        Span::styled(shown, if text.is_empty() { t.dim_style() } else { t.strong_style() })
    }
}

pub(crate) fn available(app: &App, asset: &SwapAsset) -> Option<String> {
    let p = app.eco.portfolio.value()?;
    let id = match asset {
        SwapAsset::Quai => "quai".to_string(),
        SwapAsset::Token { address, .. } => address.clone(),
    };
    let r = p.rows.iter().find(|r| r.key.id() == id)?;
    Some(amount::group_thousands(&amount::format_amount_short(r.amount(), r.decimals, 6)))
}

pub(crate) fn asset_chip(app: &App, t: &Theme, asset: Option<&SwapAsset>) -> Vec<Span<'static>> {
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
                        .value()
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
                Span::styled(
                    format!(
                        "[{} {}{}]",
                        a.symbol(),
                        if verified { t.icon(Icon::Ok) } else { t.icon(Icon::Warning) },
                        t.icon(Icon::Dropdown)
                    ),
                    t.strong_style(),
                ),
            ]
        }
        None => vec![Span::styled(format!("[pick {}]", t.icon(Icon::Dropdown)), t.dim_style())],
    }
}

pub(crate) fn step_line(t: &Theme, steps: &[&str], current: usize) -> Line<'static> {
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
        spans.push(Span::styled(format!("{}{} {s}", if i < current { t.icon(Icon::Ok) } else { "" }, i + 1), style));
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
    let network = app.net();
    if network.as_ref().is_none_or(|n| n.ecosystem.quainance_router.is_none()) {
        let block = panel(t, "swap", true);
        let inner = block.inner(area);
        f.render_widget(block, area);
        empty(f, inner, t, Icon::Swap, "No swap router on this network (mainnet only).", &[("g c", "convert"), ("g w", "wrap")]);
        return;
    }
    let quote = if app.swap_quote_current() { card.quote.as_ref() } else { None };
    let ok_quote = quote.and_then(|q| q.as_ref().ok());
    // The token's bonding curve, when it pays more than the exchanges or they cannot fill it.
    let on_curve = if app.swap_quote_current() { app.swap_uses_curve() } else { None };
    let curve_out = |o: &wallet_core::curve::CurveOffer, decimals: u8| {
        amount::group_thousands(&amount::format_amount_short(o.amount().unwrap_or_default(), decimals, 6))
    };
    let to_decimals = card.to.as_ref().map_or(18, |a| a.decimals());
    let receive = match (&on_curve, ok_quote) {
        (Some(o), _) => Span::styled(format!("≈ {} on the curve", curve_out(o, to_decimals)), t.strong_style()),
        (None, Some(q)) => Span::styled(
            format!(
                "≈ {}",
                amount::group_thousands(&amount::format_amount_short(q.amount_out.parse().unwrap_or_default(), q.to.decimals(), 6))
            ),
            t.strong_style(),
        ),
        (None, None) if !card.amount.is_empty() => Span::styled(format!("{} quoting…", spinner()), t.dim_style()),
        (None, None) => Span::styled("—", t.dim_style()),
    };
    let mut c = Card::new();
    if let Some(line) = acting_line(app, t, "from") {
        c.line(line);
    }
    c.line(Line::from(Span::styled("you pay", t.dim_style())));
    c.field(t, 0, card.field, "token", asset_chip(app, t, Some(&card.from)));
    c.field(
        t,
        1,
        card.field,
        "amount",
        vec![
            amount_span(t, &card.amount, card.field == 1),
            Span::styled(available(app, &card.from).map(|a| format!("   available {a}")).unwrap_or_default(), t.dim_style()),
        ],
    );
    c.line(preset_line(t, card.preset));
    c.line(Line::from(Span::styled("you receive", t.dim_style())));
    c.field(t, 2, card.field, "token", asset_chip(app, t, card.to.as_ref()));
    c.line(card_row(t, false, "amount", vec![receive]));
    c.line(Line::from(""));
    c.field(t, 3, card.field, "slippage", cycler(t, format!("{:.2}%", f64::from(card.slippage_bps) / 100.0), card.field == 3));
    c.field(t, 4, card.field, "deadline", cycler(t, format!("{} min", card.deadline_minutes), card.field == 4));
    if let Some(o) = &on_curve {
        c.line(Line::from(""));
        let what = if o.sell {
            format!("enter · sell {} to its bonding curve", o.symbol)
        } else {
            format!("enter · buy {} on its bonding curve", o.symbol)
        };
        c.action(Line::from(Span::styled(what, t.strong_style().fg(t.focus))), crossterm::event::KeyCode::Enter);
    } else if let Some(q) = ok_quote
        && (q.approval_needed || card.approving)
    {
        c.line(Line::from(""));
        c.line(step_line(t, &[&format!("approve exact {}", q.pay_text()), "swap"], 0));
        if card.approving {
            c.line(Line::from(Span::styled(format!("{} waiting for the approval to confirm…", spinner()), t.dim_style())));
        }
    } else if ok_quote.is_some() {
        c.line(Line::from(""));
        c.action(Line::from(Span::styled("enter · review the swap", t.strong_style().fg(t.focus))), crossterm::event::KeyCode::Enter);
    }
    // Wide and tall: the form takes what it needs and the pair's chart fills the rest.
    let form_h = c.lines.len() as u16 + 2;
    let (form, chart) = if !stacked && left.height >= form_h + 12 {
        let [a, b] = Layout::vertical([Constraint::Length(form_h), Constraint::Min(10)]).areas(left);
        (a, Some(b))
    } else {
        (left, None)
    };
    // Beside Markets (the trader layout) the card is lit only while its screen has the keys.
    let block = panel(t, "exchange · swap on Quainance", app.screen != Screen::Markets);
    if app.screen == Screen::Swap {
        c.hits(app, block.inner(form));
    }
    f.render_widget(Paragraph::new(c.lines).block(block), form);
    let pair = app.swap_pool();
    if let Some(area) = chart {
        draw_swap_chart(f, app, t, area, pair.as_ref());
    }

    let block = panel(t, "quote", false);
    let inner = block.inner(right);
    f.render_widget(block, right);
    let mut q_lines = Vec::new();
    // A token with a curve trades in two places: say which, and which pays more for this amount.
    let offer = if app.swap_quote_current() { card.curve.as_ref().and_then(|r| r.as_ref().ok()) } else { None };
    if let Some(o) = offer
        && let Some(to) = &card.to
    {
        let curve_pool = card.to.as_ref().and(app.eco.markets_view.pools.as_ref()).and_then(|r| r.as_ref().ok()).and_then(|(pools, _)| {
            pools.iter().find(|p| p.venue == wallet_core::markets::Venue::Curve && p.address.eq_ignore_ascii_case(&o.curve))
        });
        let depth = curve_pool
            .and_then(|p| app.row_tvl_usd(p))
            .map(|v| format!(" · {} deep", wallet_core::swap::usd_compact(v)))
            .unwrap_or_default();
        let theirs = format!("≈ {} {}", curve_out(o, to.decimals()), to.symbol());
        let routed = ok_quote.and_then(|q| U256::from_str_radix(&q.amount_out, 10).ok());
        let vs = match routed {
            Some(r) if !r.is_zero() => {
                let (c, r) =
                    (o.amount().unwrap_or_default().to_string().parse::<f64>().unwrap_or(0.0), r.to_string().parse::<f64>().unwrap_or(0.0));
                let pct = (c / r - 1.0) * 100.0;
                if pct >= 0.0 { format!("  {pct:.1}% more than the exchanges") } else { format!("  {:.1}% less than the exchanges", -pct) }
            }
            _ => "  the exchanges cannot fill this".to_string(),
        };
        let style = if on_curve.is_some() { t.strong_style() } else { t.text_style() };
        q_lines.push(kv(t, "curve", Span::styled(format!("{theirs}{vs}"), style)));
        q_lines.push(kv(
            t,
            "",
            Span::styled(
                format!(
                    "{} trades on its bonding curve{depth}{}",
                    o.symbol,
                    if on_curve.is_some() { " · enter trades there" } else { " · the exchanges pay more here" }
                ),
                t.dim_style(),
            ),
        ));
        q_lines.push(Line::from(""));
    }
    match quote {
        // The exchanges refused, but the curve fills it: a note, not an error.
        Some(Err(e)) if on_curve.is_some() => {
            q_lines.push(kv(t, "exchanges", Span::styled(app::friendly_error(e), t.dim_style())));
        }
        Some(Ok(q)) => {
            q_lines.push(kv(t, if offer.is_some() { "exchanges" } else { "route" }, Span::raw(q.route_text())));
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
            let high = q.impact_bps >= wallet_core::swap::IMPACT_WARN_BPS;
            let impact_color = if high { t.attention } else { t.ok };
            // Said in words as well as color: a high impact must read as high without either.
            q_lines.push(kv(
                t,
                "impact",
                Span::styled(
                    format!(
                        "{}{}  {:.2}%{}",
                        "■".repeat(filled),
                        "□".repeat(10 - filled),
                        q.impact_bps as f64 / 100.0,
                        if high { format!(" · {}high", t.lead(Icon::Warning)) } else { String::new() }
                    ),
                    Style::default().fg(impact_color),
                ),
            ));
            q_lines.push(kv(t, "fee", Span::raw(format!("{:.1}% LP · gas at review", q.fee_bps as f64 / 100.0))));
            q_lines.push(kv(t, "pools", Span::raw(q.pools.iter().map(|p| short_address(&p.pair)).collect::<Vec<_>>().join(" · "))));
            if let Some(l) = q.liquidity_text() {
                let thin = q.pools.iter().any(|p| p.tvl_usd.is_some_and(|v| v < wallet_core::swap::THIN_POOL_USD));
                let l = if thin { format!("{l} · {}thin pool", t.lead(Icon::Warning)) } else { l };
                q_lines.push(kv(t, "liquidity", Span::styled(l, if thin { Style::default().fg(t.attention) } else { t.text_style() })));
            }
            let venues: Vec<&str> = q.legs.iter().map(|l| l.venue.label()).collect();
            let venues = if venues.is_empty() { "Quainance".to_string() } else { venues.join(", then ") };
            q_lines.push(kv(
                t,
                "router",
                Span::styled(format!("{} {venues} (pinned bytecode)", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
            ));
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
        Some(Err(e)) => q_lines
            .push(Line::from(Span::styled(format!("{} {}", t.icon(Icon::Danger), app::friendly_error(e)), Style::default().fg(t.danger)))),
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
pub(crate) fn preset_line(t: &Theme, preset: Option<u8>) -> Line<'static> {
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
pub(crate) fn draw_swap_chart(f: &mut Frame, app: &App, t: &Theme, area: Rect, pair: Option<&(wallet_core::markets::Pool, bool)>) {
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
            empty(f, inner, t, Icon::Pool, "No pool trades this pair directly; the router finds a path through WQUAI.", &[]);
        }
        return;
    };
    let chart_w = inner.width.saturating_sub(11).max(8);
    let step = if chart_w as usize / 2 >= 24 { 2 } else { 1 };
    let n = ((chart_w / step) as usize).min(super::super::eco::MARKET_CANDLES);
    let cs = app.chart_candles(pool, *pay0, super::super::eco::SWAP_CHART_BUCKET, n);
    if cs.is_empty() {
        empty_state(f, inner, t, spinner(), "Reading the pair's history…", &[]);
    } else {
        draw_candles(f, t, inner, &cs, step, super::super::eco::SWAP_CHART_BUCKET, app.pointer.at);
    }
}

/// QUAI ⇄ Qi has two markets: the protocol conversion (one transaction, the controller's rate,
/// output locked for weeks) and the market route through Quainance (wrap, swap, unwrap: several
/// transactions and LP costs, spendable in minutes). Both are quoted for the amount on the card
/// and either can be started here.
/// With more than one account, the card says which one it acts from (and that `@` changes it).
fn acting_line(app: &App, t: &Theme, verb: &str) -> Option<Line<'static>> {
    if app.dash.accounts.len() < 2 {
        return None;
    }
    let a = app.dash.active_account()?;
    Some(Line::from(vec![
        Span::styled(format!("{verb} "), t.dim_style()),
        Span::styled(a.label.clone(), t.strong_style()),
        Span::styled(format!(" · {}", wallet_core::session::short_address(&a.address)), t.dim_style()),
        Span::styled("   @ changes it", t.dim_style()),
    ]))
}

pub fn draw_convert_card(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let card = &app.eco.convert;
    let [left, right] = if area.width < 100 {
        Layout::vertical([Constraint::Length(12), Constraint::Min(8)]).areas(area)
    } else {
        Layout::horizontal([Constraint::Percentage(46), Constraint::Percentage(54)]).areas(area)
    };
    let (pay, get) = if card.qi_to_quai { ("Qi", "QUAI") } else { ("QUAI", "Qi") };
    let avail = if card.qi_to_quai {
        app.dash.qi.as_ref().map(|q| format!("{} Qi", num::qi(q.balance.spendable)))
    } else {
        app.dash.active_account().map(|a| format!("{} QUAI", num::short(a.balance, 18, 4)))
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
    let mut c = Card::new();
    if let Some(line) = acting_line(app, t, if card.qi_to_quai { "to" } else { "from" }) {
        c.line(line);
    }
    c.field(
        t,
        0,
        card.field,
        "pair",
        vec![
            images::native_span(app, t, pay),
            Span::styled(format!(" {pay} → "), t.strong_style()),
            images::native_span(app, t, get),
            Span::styled(format!(" {get}"), t.strong_style()),
            Span::styled(" ‹›", if card.field == 0 { Style::default().fg(t.focus) } else { t.dim_style() }),
        ],
    );
    c.field(
        t,
        1,
        card.field,
        "you pay",
        vec![
            amount_span(t, &card.amount, card.field == 1),
            Span::styled(format!(" {pay}"), t.dim_style()),
            Span::styled(avail.map(|a| format!("   available {a}")).unwrap_or_default(), t.dim_style()),
        ],
    );
    c.field(
        t,
        2,
        card.field,
        "slippage",
        // Not chosen and not yet suggested: say so, rather than show a placeholder as a setting.
        [
            cycler(
                t,
                if card.slippage_bps == 0 && card.quote.is_none() {
                    "auto".to_string()
                } else {
                    format!("{:.2}%", f64::from(slippage) / 100.0)
                },
                card.field == 2,
            ),
            vec![Span::styled(
                if card.slippage_bps == 0 { " (conversion · suggested by the quote)" } else { " (conversion)" },
                t.dim_style(),
            )],
        ]
        .concat(),
    );
    c.field(t, 3, card.field, "route", cycler(t, route_name.to_string(), card.field == 3));
    c.line(Line::from(""));
    let ready = comparison.is_some_and(|c| if card.market { c.market.usable() } else { c.protocol.usable() });
    c.action(
        Line::from(vec![
            key(t, "enter"),
            Span::raw(match (card.market, ready) {
                (true, true) => "start the market route",
                (true, false) => "quoting…",
                (false, _) if card.quote.is_some() => "review the conversion",
                (false, _) => "quote the conversion",
            }),
            Span::styled("   r route   f flip", t.dim_style()),
        ]),
        crossterm::event::KeyCode::Enter,
    );
    let prose = if card.market {
        "Each step is reviewed on its own; the next opens when the previous confirms."
    } else {
        "Conversions in one prime block share a discount; beyond your slippage they refund (fee spent)."
    };
    // Wrapped here: a trimming paragraph wrap would also eat each field's focus-marker column.
    let block = panel(t, "exchange · convert QUAI ↔ Qi", true);
    let inner = block.inner(left);
    for l in super::super::ui::textwrap(prose, inner.width.max(1) as usize) {
        c.line(Line::from(Span::styled(l, t.dim_style())));
    }
    c.hits(app, inner);
    f.render_widget(Paragraph::new(c.lines).block(block), left);

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
pub(crate) fn market_quotes(app: &App, t: &Theme, c: &wallet_core::qi_market::Comparison, width: u16, big: bool) -> Vec<Line<'static>> {
    let card = &app.eco.convert;
    let best = c.better().map(|r| r.name.clone());
    let mut q = Vec::new();
    for (market, route) in [(false, &c.protocol), (true, &c.market)] {
        let selected = market == card.market;
        let is_best = best.as_deref() == Some(route.name.as_str());
        let bar = Span::styled(if selected { "▌" } else { " " }, Style::default().fg(t.focus));
        let (icon, kind, colour) = if market { ("≋", "MARKET", t.quai) } else { (t.icon(Icon::Protocol), "PROTOCOL", t.qi) };
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
        let tick =
            Span::styled(if is_best { format!("  {} pays more", t.icon(Icon::Ok)) } else { String::new() }, Style::default().fg(t.ok));
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
/// Trading PnL in QUAI: the totals, a row per token traded, what the focused one needs said,
/// and the latest trades behind it.
pub fn draw_pnl(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::pnl::{price_text, quai_text, signed_text, units_text};
    let block = panel(t, "trading PnL · in QUAI", true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let pnl = match app.eco.pnl.latest() {
        None => return empty_state(f, inner, t, spinner(), "Reading this wallet's trades…", &[]),
        Some(Err(e)) => return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        Some(Ok(p)) if p.fills.is_empty() => {
            return empty(
                f,
                inner,
                t,
                Icon::Trade,
                "No trades yet. Swaps and curve trades made from this wallet show up here, with their cost and gain in QUAI.",
                &[(super::super::keymap::chord(Screen::Swap).as_str(), "trade")],
            );
        }
        Some(Ok(p)) => p,
    };
    // Gains and losses are money moving, not states: up and down, never ok and danger.
    let tone = |v: f64| {
        if v > 0.00005 {
            Style::default().fg(t.up)
        } else if v < -0.00005 {
            Style::default().fg(t.down)
        } else {
            t.text_style()
        }
    };
    let trade_rows = pnl.fills.len().min(8) as u16;
    let [summary, positions, trades] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(4),
        Constraint::Length(if inner.height >= 18 { trade_rows + 2 } else { 0 }),
    ])
    .areas(inner);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("net ", t.dim_style()),
                Span::styled(format!("{} QUAI", num::minus(signed_text(pnl.net))), tone(pnl.net).add_modifier(Modifier::BOLD)),
                Span::styled(if app.eco.pnl.loading() { "  refreshing…" } else { "" }, t.dim_style()),
            ]),
            Line::from(vec![
                Span::styled("realized ", t.dim_style()),
                Span::styled(num::minus(signed_text(pnl.realized)), tone(pnl.realized)),
                Span::styled("  unrealized ", t.dim_style()),
                Span::styled(num::minus(signed_text(pnl.unrealized)), tone(pnl.unrealized)),
                Span::styled(format!("  fees {}  ·  {}", quai_text(pnl.fees), amount::count(pnl.fills.len(), "trade")), t.dim_style()),
            ]),
        ]),
        summary,
    );
    // Columns earn their place by width: the price and trade count go first, then average cost.
    let (wide, medium) = (positions.width >= 96, positions.width >= 76);
    let header = format!(
        "  {:<10} {:>10} {}{}{:>10} {:>11} {:>11}{}",
        "token",
        "held",
        if medium { format!("{:>11} ", "avg cost") } else { String::new() },
        if wide { format!("{:>11} ", "price") } else { String::new() },
        "value",
        "unrealized",
        "realized",
        if wide { format!(" {:>6}", "trades") } else { String::new() },
    );
    let mut lines = vec![Line::from(Span::styled(header, t.dim_style()))];
    let list = &pnl.positions;
    let selected = app.selected.min(list.len().saturating_sub(1));
    // Room for the focused token's notes under the table.
    let room = (positions.height as usize).saturating_sub(3).max(1);
    let start = app.list_window(app.main_list(), selected, list.len(), room);
    app.hits.borrow_mut().rows(app.main_list(), Rect { y: positions.y + 1, height: room as u16, ..positions }, start, list.len(), |_| None);
    for (i, p) in list.iter().enumerate().skip(start).take(room) {
        let focused = i == selected;
        let style = if focused { t.selected() } else { t.text_style() };
        let held = if p.open > 0.0 { units_text(p.open) } else { "closed".into() };
        let dash = || "—".to_string();
        let mut spans = vec![
            Span::styled(if focused { "▌ " } else { "  " }, Style::default().fg(t.focus)),
            Span::styled(
                format!("{:<10} ", truncate(&format!("{}{}", p.symbol, if p.estimated { " ~" } else { "" }), 10)),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{held:>10} "), if p.open > 0.0 { style } else { t.dim_style() }),
        ];
        if medium {
            spans.push(Span::styled(format!("{:>11} ", p.avg_cost.map(price_text).unwrap_or_else(dash)), t.dim_style()));
        }
        if wide {
            spans.push(Span::styled(format!("{:>11} ", p.mark.map(price_text).unwrap_or_else(dash)), t.dim_style()));
        }
        spans.extend([
            Span::styled(format!("{:>10} ", p.value.map(quai_text).unwrap_or_else(dash)), style),
            Span::styled(
                format!("{:>11} ", p.unrealized.map(|v| num::minus(signed_text(v))).unwrap_or_else(dash)),
                p.unrealized.map_or(t.dim_style(), tone),
            ),
            Span::styled(format!("{:>11}", num::minus(signed_text(p.realized))), tone(p.realized)),
        ]);
        if wide {
            spans.push(Span::styled(format!(" {:>6}", p.trades), t.dim_style()));
        }
        lines.push(Line::from(spans));
    }
    // What the focused token's figures leave out, said once, under the table.
    if let Some(p) = list.get(selected) {
        let mut notes = Vec::new();
        if p.open > 0.0 && p.mark.is_none() {
            notes.push("no WQUAI pool or curve prices it, so it stands at cost".to_string());
        }
        if p.moved_out > 0.0 {
            notes.push(format!("{} left the wallet other than by a trade, removed at cost", units_text(p.moved_out)));
        }
        if p.unmatched_sold > 0.0 {
            notes.push(format!("{} sold with no buy recorded here: no gain claimed on it", units_text(p.unmatched_sold)));
        }
        if p.incomplete_basis {
            notes.push("part of its cost is unknown".into());
        }
        if p.estimated {
            notes.push("~ some figures are the review's; the receipt did not record them".into());
        }
        if !notes.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(format!("{}: {}", p.symbol, notes.join(" · ")), t.dim_style())));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), positions);
    if trades.height > 0 {
        let mut lines = vec![Line::from(Span::styled("latest trades", t.dim_style()))];
        // The tokens take what the date, side and QUAI columns leave.
        let width = (trades.width as usize).saturating_sub(40).clamp(20, 48);
        for fill in pnl.fills.iter().take(trade_rows as usize) {
            let when = chrono::DateTime::from_timestamp(fill.at as i64, 0)
                .map(|at| at.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let tokens = fill
                .legs
                .iter()
                .map(|l| format!("{}{} {}", if l.units > 0.0 { "+" } else { "" }, num::minus(units_text(l.units)), l.symbol))
                .collect::<Vec<_>>()
                .join("  ");
            let side = fill.side();
            lines.push(Line::from(vec![
                Span::styled(format!("  {when}  "), t.dim_style()),
                Span::styled(format!("{side:<5}"), Style::default().fg(if side == "sell" { t.down } else { t.up })),
                Span::styled(format!("{:<width$} ", truncate(&tokens, width)), t.text_style()),
                // What a trade paid or fetched is a flow, not a gain: it is not coloured as one.
                Span::styled(
                    if fill.quai != 0.0 { format!("{} QUAI", num::minus(signed_text(fill.quai))) } else { String::new() },
                    t.dim_style(),
                ),
            ]));
        }
        f.render_widget(Paragraph::new(lines), trades);
    }
}

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
    let heading = match app.eco.launches.shown() {
        Some(Ok(list)) => format!("launch zone · {}", amount::count(list.len(), "token")),
        _ => "launch zone".into(),
    };
    let block = panel(t, &heading, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    match app.eco.launches.shown() {
        None => return empty_state(f, inner, t, spinner(), "Reading Quainance's launch zone…", &[]),
        Some(Err(e)) if rows.is_empty() => {
            return empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]);
        }
        _ if rows.is_empty() => {
            return empty(
                f,
                inner,
                t,
                Icon::Launch,
                "No launches yet. New tokens start here on a bonding curve, then graduate to Markets.",
                &[("R", "reload")],
            );
        }
        _ => {}
    }
    let quai_usd = app.eco.portfolio.value().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
    let now = wallet_core::registry::now();
    // The name column and the age earn their place from 86 columns: below that the symbol and
    // numbers are what fit.
    let wide = inner.width >= 86;
    let height = inner.height.saturating_sub(2) as usize;
    let selected = app.selected.min(rows.len() - 1);
    let start = app.list_window(app.main_list(), selected, rows.len(), height);
    app.hits.borrow_mut().rows(app.main_list(), Rect { y: inner.y + 1, height: height as u16, ..inner }, start, rows.len(), |_| None);
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
            Phase::Pooled => t.link,
            Phase::Other => t.dim,
        };
        let price = l.price_quai.map(super::super::views::fmt_price).unwrap_or_else(|| "—".into());
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
    if let Some(Err(e)) = app.eco.launches.shown() {
        lines.push(Line::from(Span::styled(
            format!("{} last refresh failed: {}", t.icon(Icon::Danger), truncate(&app::friendly_error(e), 60)),
            Style::default().fg(t.danger),
        )));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

/// Where the focused token stands on its bonding curve: the price curve from launch to graduation,
/// the part already raised filled in, a marker at the current point, and what that means in
/// numbers — raised against the target, how much of the allocation is sold, the price now, and what
/// this wallet holds and is owed.
pub(crate) fn draw_curve(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let rows = app.launch_rows();
    let Some(l) = rows.get(app.selected) else { return };
    let block = panel(t, &format!("{} on its bonding curve", l.symbol), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(m) = app.focused_curve().filter(|m| m.token == l.token) else {
        let text = match app.eco.curves.get(&l.token).and_then(|r| r.shown()) {
            Some(Err(e)) => format!("{} {}", t.icon(Icon::Danger), truncate(&app::friendly_error(e), 70)),
            _ => format!("{} reading the curve…", spinner()),
        };
        f.render_widget(Paragraph::new(Span::styled(text, t.dim_style())), inner);
        return;
    };
    let quai_usd = app.eco.portfolio.value().and_then(|p| p.prices.as_ref()).and_then(|b| b.quai_usd);
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
                // A HartiiLabs price is the reserve spot when the reserves reproduce the curve's
                // own quote, and the inverted one-QUAI quote only when they do not.
                if l.venue_kind == Some(wallet_core::capabilities::Family::HartiiCurve)
                    && l.price_basis == wallet_core::markets::PriceBasis::OneQuaiBuyQuote
                {
                    "1 QUAI quote "
                } else {
                    "price   "
                },
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
            Span::styled(format!("{} QUAI to claim · c", num::short(m.claimable, 18, 4)), Style::default().fg(t.ok)),
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
        Paragraph::new(Line::from(vec![Span::styled(" ".repeat(label_x as usize), t.dim_style()), Span::styled(label, t.strong_style())])),
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
    let mut c = Card::new();
    if let Some(line) = acting_line(app, t, "account") {
        c.line(line);
    }
    c.field(t, 0, card.field, "pair", cycler(t, label.to_string(), card.field == 0));
    if card.mode != 1 {
        c.field(
            t,
            1,
            card.field,
            "amount",
            vec![amount_span(t, &card.amount, card.field == 1), Span::styled(format!(" {}", num::unit(unit)), t.dim_style())],
        );
    }
    // What the selected mode can use: spendable Qi, unclaimed backing, whole WQI, or WQUAI.
    let w = app.dash.wrap.as_ref();
    let parse = |v: Option<&String>| v.and_then(|v| v.parse::<U256>().ok());
    let available = match card.mode {
        0 => app.dash.qi.as_ref().map(|q| format!("{} Qi spendable", num::qi(q.balance.spendable))),
        1 => parse(w.and_then(|w| w.unclaimed_qits.as_ref())).map(|q| {
            if q.is_zero() {
                "nothing to claim yet (backing arrives after the wrap settles)".into()
            } else {
                format!("{} Qi ready to claim", num::qi(q))
            }
        }),
        2 => w.and_then(|w| w.wqi_qi.clone()).map(|v| format!("{v} WQI (redeem whole Qi)")),
        3 => app.dash.active_account().map(|a| format!("{} QUAI", num::short(a.balance, 18, 4))),
        _ => parse(w.and_then(|w| w.wquai_atoms.as_ref())).map(|v| format!("{} WQUAI", num::short(v, 18, 4))),
    };
    if let Some(text) = available {
        c.line(card_row(t, false, "available", vec![Span::styled(text, t.dim_style())]));
    }
    // Wrapped here rather than by the paragraph, so the lines below keep the rows the pointer
    // was told about.
    let block = panel(t, "exchange · wrap", true);
    let inner = block.inner(left);
    for l in super::super::ui::textwrap(story, inner.width.max(1) as usize) {
        c.line(Line::from(Span::styled(l, t.dim_style())));
    }
    c.line(Line::from(""));
    c.action(Line::from(vec![key(t, "enter"), Span::raw("review")]), crossterm::event::KeyCode::Enter);
    if card.mode <= 1 {
        c.line(step_line(t, &["wrap Qi", "wait for settlement", "claim WQI"], if card.mode == 0 { 0 } else { 2 }));
    }
    c.hits(app, inner);
    f.render_widget(Paragraph::new(c.lines).block(block), left);
    let mut s = Vec::new();
    match &app.dash.wrap {
        Some(w) => {
            s.push(kv(t, "account", Span::styled(short_address(&w.account), Style::default().fg(t.link))));
            let network = app.net();
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
                        .map(|v| format!("{} Qi", num::qi(v.parse().unwrap_or_default())))
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
                        .map(|v| format!("{} WQUAI", num::short(v.parse().unwrap_or_default(), 18, 4)))
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
    for o in app.dash.ops.iter().filter(|o| o.kind.as_str().contains("wrap") || o.kind.as_str().contains("claim")).take(5) {
        s.push(Line::from(vec![
            Span::styled(format!("{} ", status_glyph(t, o.status)), t.dim_style()),
            Span::raw(truncate(&describe(o), 40)),
            Span::styled(format!("  {}", ago(o.created)), t.dim_style()),
        ]));
    }
    f.render_widget(Paragraph::new(s).block(panel(t, "wrapped balances", false)), right);
}

pub fn draw_token_picker(f: &mut Frame, app: &App, t: &Theme, area: Rect, query: &str, selected: usize, pay: bool) {
    use super::super::eco::{PickerEntry, RouteState};
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
    let start = app.lists.borrow_mut().entry(crate::tui::hit::ListId::TokenPicker).or_default().window(selected, entries.len(), height);
    app.hits.borrow_mut().rows(
        crate::tui::hit::ListId::TokenPicker,
        Rect { y: area.y + 2, height: height as u16, ..area },
        start,
        entries.len(),
        |_| None,
    );
    for (i, entry) in entries.iter().enumerate().skip(start).take(height) {
        let PickerEntry { asset, info, verified, holders, icon, route, qi } = entry;
        let active = i == selected;
        let dead = matches!(route, RouteState::Dead);
        let (addr, lookalike) = match asset {
            _ if *qi => ("native".to_string(), false),
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
            _ if *qi => images::native_span(app, t, "qi"),
            SwapAsset::Quai => images::native_span(app, t, "quai"),
            SwapAsset::Token { address, symbol, .. } => {
                images::badge_span(app, t, icon.clone().or_else(|| app.asset_icon_url(address)).as_deref(), symbol, address)
            }
        };
        // The route badge carries the hop count and the thinnest pool on the way: enough to tell
        // "this will cost you three fees" from "this pool holds four dollars".
        let route_span = match route {
            RouteState::Unknown => Span::raw(""),
            RouteState::Dead => Span::styled(format!("  {} no route", t.icon(Icon::Danger)), Style::default().fg(t.danger)),
            RouteState::Fillable(info) if info.thin() => Span::styled(
                format!(
                    "  {} {} · {} pool",
                    t.icon(Icon::Warning),
                    amount::count(info.hops(), "hop"),
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
            Span::styled(format!("{:<10}", truncate(if *qi { "Qi" } else { asset.symbol() }, 10)), style.add_modifier(Modifier::BOLD)),
            if *verified {
                Span::styled(format!(" {} ", t.icon(Icon::Ok)), Style::default().fg(t.ok))
            } else {
                Span::styled(format!(" {} ", t.icon(Icon::Warning)), Style::default().fg(t.attention))
            },
            Span::styled(format!("{addr:<14}"), t.dim_style()),
            Span::styled(format!("{:<16}", truncate(info, 16)), style),
            Span::styled(holders.map(|h| format!("{h} holders")).unwrap_or_default(), t.dim_style()),
            route_span,
            Span::styled(
                if lookalike { format!("  {} lookalike symbol", t.icon(Icon::Warning)) } else { String::new() },
                Style::default().fg(t.danger),
            ),
        ]));
    }
    if entries.is_empty() {
        lines.push(Line::from(Span::styled("no matching tokens", t.dim_style())));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(t.raised)), area);
    let _ = f;
}
