//! NFTs: what the wallet holds, the collections to explore, listings and details.

use super::*;

// ---------------------------------------------------------------- NFTs

pub(crate) fn nft_tile(
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
        Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry"), ("g d", "data sources")]),
        Some(Ok(items)) if items.is_empty() => empty_state(
            f,
            inner,
            t,
            "◧",
            "No NFTs in this wallet yet. Browse collections, or what is for sale right now.",
            &[("g e", "explore"), ("g i", "listings")],
        ),
        Some(Ok(items)) => {
            let (tile_w, tile_h) =
                if app.caps.tier == super::super::terminal::Tier::Text || app.plain { (22u16, 4u16) } else { (18u16, 10u16) };
            let cols = (inner.width / tile_w).max(1) as usize;
            *app.eco.grid_columns.borrow_mut() = cols;
            let rows_visible = (inner.height / tile_h).max(1) as usize;
            let row_of_selected = app.selected / cols;
            // The grid scrolls by whole rows of tiles, and only when the selection leaves it.
            let first_row = app.list_window(app.main_list(), row_of_selected, items.len().div_ceil(cols), rows_visible);
            for (i, n) in items.iter().enumerate().skip(first_row * cols).take(rows_visible * cols) {
                let (row, col) = ((i / cols - first_row) as u16, (i % cols) as u16);
                let rect = Rect::new(inner.x + col * tile_w, inner.y + row * tile_h, tile_w.saturating_sub(2), tile_h.saturating_sub(1));
                app.hits.borrow_mut().add(
                    rect,
                    crate::tui::hit::Target::Row {
                        list: app.main_list(),
                        index: i,
                        key: Some(format!("{}:{}", n.item.contract, n.item.token_id)),
                    },
                );
                let sub = match app.my_listing(&n.item.contract, &n.item.token_id) {
                    Some(l) => Span::styled(format!("{} listed {}", t.icon(Icon::Listed), app.listing_price(&l)), t.strong_style()),
                    None => Span::styled(
                        format!(
                            "{} owned{}",
                            t.icon(Icon::Ok),
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
        None => format!("collections{market}{sorted} · {} sort · / search", crate::tui::keymap::key_of(crate::tui::keymap::Verb::Sort)),
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
        Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        Some(Ok(_)) => {
            let list = app.eco.collections_filtered();
            if list.is_empty() {
                empty(f, inner, t, Icon::Search, "No collections match.", &[("/", "search")]);
                return;
            }
            let row_h = if app.caps.tier == super::super::terminal::Tier::Text || app.plain { 1u16 } else { 2u16 };
            let visible = (inner.height.saturating_sub(1) / row_h).max(1) as usize;
            let offset = app.list_window(app.main_list(), app.selected, list.len(), visible);
            for (k, c) in list.iter().enumerate().skip(offset).take(visible) {
                let y = inner.y + 1 + ((k - offset) as u16) * row_h;
                app.hits.borrow_mut().add(
                    Rect::new(inner.x, y, inner.width, row_h),
                    crate::tui::hit::Target::Row { list: app.main_list(), index: k, key: Some(c.address.clone()) },
                );
            }
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
pub(crate) fn draw_listings_table(
    f: &mut Frame,
    app: &App,
    t: &Theme,
    area: Rect,
    list: &[wallet_core::market::Listing],
    selected: Option<usize>,
    id: crate::tui::hit::ListId,
) {
    let pictures = app.caps.tier != super::super::terminal::Tier::Text && !app.plain && app.config.images && area.width >= 96;
    let row_h: u16 = if pictures { 2 } else { 1 };
    let height = (area.height.saturating_sub(1) / row_h).max(1) as usize;
    let offset = match selected {
        Some(sel) => app.list_window(id, sel, list.len(), height),
        None => 0,
    };
    let shown: Vec<(usize, &wallet_core::market::Listing)> = list.iter().enumerate().skip(offset).take(height).collect();
    // Rows are clickable where the list is the screen's own (the collection detail keeps a
    // cursor of its own, which the per-screen state in phase 5 brings under the same rule).
    if selected.is_some() && matches!(id, crate::tui::hit::ListId::Screen(..)) {
        let mut hits = app.hits.borrow_mut();
        for (k, &(i, l)) in shown.iter().enumerate() {
            let rect = Rect::new(area.x, area.y + 1 + k as u16 * row_h, area.width, row_h);
            let key = Some(format!("{}:{}", l.contract, l.token_id));
            hits.add(rect, crate::tui::hit::Target::Row { list: id, index: i, key });
        }
    }
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
            Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[(".", "everyone's")]),
            Some(Ok(list)) if list.is_empty() => {
                empty(f, inner, t, Icon::Nfts, "You have nothing listed.", &[("g n", "collected · space to list"), (".", "everyone's")])
            }
            Some(Ok(_)) => {
                let visible = app.eco.visible_listings();
                draw_listings_table(f, app, t, inner, &visible, Some(app.selected), app.main_list())
            }
        }
        return;
    }
    match app.eco.listings.get(&None) {
        None => empty_state(f, inner, t, spinner(), "Loading listings…", &[]),
        Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[("R", "retry")]),
        Some(Ok(list)) if list.is_empty() => empty(f, inner, t, Icon::Nfts, "No active listings.", &[]),
        Some(Ok(_)) => {
            let visible = app.eco.visible_listings();
            draw_listings_table(f, app, t, inner, &visible, Some(app.selected), app.main_list())
        }
    }
}

pub(crate) fn draw_nft_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str, token_id: &str) {
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
        lines.push(kv(t, "owner", Span::styled(format!("{} you (checked on-chain)", t.icon(Icon::Ok)), Style::default().fg(t.ok))));
        if let Some(mine) = app.my_listing(contract, token_id) {
            let price = Span::styled(app.listing_price(&mine), t.strong_style());
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
                lines.push(kv(
                    t,
                    "on-chain",
                    Span::styled(format!("{} ask valid · seller owns it", t.icon(Icon::Ok)), Style::default().fg(t.ok)),
                ));
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
                    lines.push(Line::from(Span::styled(format!("{} {p}", t.icon(Icon::Danger)), Style::default().fg(t.danger))));
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

pub(crate) fn draw_collection_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str) {
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
        Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[]),
        Some(Ok(items)) if items.is_empty() => empty(f, inner, t, Icon::Nfts, "No items indexed.", &[]),
        Some(Ok(items)) => {
            let (tile_w, tile_h) =
                if app.caps.tier == super::super::terminal::Tier::Text || app.plain { (22u16, 4u16) } else { (16u16, 9u16) };
            let cols = (inner.width / tile_w).max(1) as usize;
            let rows_visible = (inner.height / tile_h).max(1) as usize;
            let first_row =
                app.list_window(crate::tui::hit::ListId::Detail, app.detail_selected / cols, items.len().div_ceil(cols), rows_visible);
            for (i, it) in items.iter().enumerate().skip(first_row * cols).take(rows_visible * cols) {
                let (row, col) = ((i / cols - first_row) as u16, (i % cols) as u16);
                let rect = Rect::new(inner.x + col * tile_w, inner.y + row * tile_h, tile_w.saturating_sub(2), tile_h.saturating_sub(1));
                app.hits
                    .borrow_mut()
                    .add(rect, crate::tui::hit::Target::Row { list: crate::tui::hit::ListId::Detail, index: i, key: None });
                let listed = app
                    .listing_for(contract, &it.token_id)
                    .map(|l| Span::styled(app.listing_price(&l), t.strong_style()))
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
        Some(Err(e)) => empty_state(f, inner, t, t.icon(Icon::Danger), &app::friendly_error(e), &[]),
        Some(Ok(list)) if list.is_empty() => empty(f, inner, t, Icon::Nfts, "Nothing listed right now.", &[]),
        Some(Ok(list)) => {
            let selected = focused.then_some(app.eco.collection_listing.min(list.len() - 1));
            draw_listings_table(f, app, t, inner, list, selected, crate::tui::hit::ListId::DetailListings)
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
pub(crate) fn draw_market_activity(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    let mine: Vec<String> = app.dash.accounts.iter().map(|a| a.address.to_lowercase()).collect();
    let block = panel(t, &format!("recent buys · {}", app.eco.nft_trades.len()), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.eco.nft_trades.is_empty() {
        return empty(f, inner, t, Icon::Nfts, "No sales through the marketplace yet.", &[]);
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
                Span::styled("you", t.strong_style())
            } else if mine.contains(&s.seller.to_lowercase()) {
                Span::styled("sold", t.strong_style())
            } else {
                Span::styled(short_address(&s.buyer), t.dim_style())
            };
            Row::new(vec![
                Cell::from(Span::styled(super::super::ui::ago_short(s.at), t.dim_style())),
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

pub(crate) fn draw_collection_sales(f: &mut Frame, app: &App, t: &Theme, area: Rect, contract: &str) {
    let c = contract.to_lowercase();
    let sales: Vec<&wallet_core::market::Trade> = app.eco.nft_trades.iter().filter(|x| x.contract == c).collect();
    let block = panel(t, &format!("sales · {}", sales.len()), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if sales.is_empty() {
        return empty(f, inner, t, Icon::Nfts, "No sales through the marketplace yet.", &[]);
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
                Cell::from(Span::styled(super::super::ui::ago(now.saturating_sub(s.at)), t.dim_style())),
            ])
        })
        .collect();
    f.render_widget(
        Table::new(rows, [Constraint::Min(10), Constraint::Length(16), Constraint::Length(9), Constraint::Length(7)]),
        list_area,
    );
}

pub(crate) fn draw_activity_detail(f: &mut Frame, app: &App, t: &Theme, area: Rect, key: &str) {
    let block = panel(t, "activity detail", true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let network = app.net();
    let mut lines = Vec::new();
    if let Some(id) = key.strip_prefix("op:")
        && let Some(op) = app.dash.ops.iter().find(|o| o.id == id)
    {
        lines.push(Line::from(Span::styled(describe(op), t.strong_style())));
        lines.push(Line::from(Span::styled(format!("{} {}", status_glyph(t, op.status), op.status.as_str()), t.text_style())));
        lines.push(Line::from(""));
        lines.extend(op_timeline(t, op));
        lines.push(Line::from(""));
        lines.push(kv(t, "operation", Span::raw(op.id.clone())));
        lines.push(kv(t, "from", Span::raw(op.account.clone())));
        lines.push(kv(t, "to", Span::raw(op.counterparty.clone())));
        for (label, value) in super::super::ui::cost_lines(app, t, Some(op), None) {
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
        for (label, value) in super::super::ui::cost_lines(app, t, None, Some(a)) {
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
pub(crate) fn op_timeline(t: &Theme, op: &wallet_core::appdb::Operation) -> Vec<Line<'static>> {
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
            Span::styled(format!("  {} ", t.icon(Icon::On)), Style::default().fg(color)),
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
