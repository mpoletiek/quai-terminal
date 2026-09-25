//! System › Wallets: every wallet on this computer.

use super::*;

// ---------------------------------------------------------------- System › Wallets

/// The wallets on this computer. Each is a separate vault with its own keys, addresses and
/// history, and only the open one is unlocked — switching drops the keys of the one leaving.
pub fn draw_wallets(f: &mut Frame, app: &App, t: &Theme, area: Rect) {
    use wallet_core::registry::WalletKind;
    use wallet_core::sdk::U256;
    // QUAI's price, to carry a wallet's summary forward to its live QUAI balance.
    let quai_usd = app.eco.portfolio.value().and_then(|p| p.rows.iter().find(|r| r.key == AssetKey::Quai)).and_then(|r| r.price_usd);
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
    // The list takes the rows it has; the selected wallet's detail goes beside it on a wide
    // terminal and under it otherwise.
    let (area, column) = super::super::ui::with_inspector(app, area);
    let list_h = if app.wallets.is_empty() { 7 } else { app.wallets.len() as u16 + 3 }.min(area.height);
    let [area, rest] = Layout::vertical([Constraint::Length(list_h), Constraint::Min(0)]).areas(area);
    if let Some(w) = app.wallets.get(app.selected) {
        match column {
            Some(column) => draw_wallet_inspector(f, app, t, column, w, value(&w.id)),
            None if rest.height >= 8 => draw_wallet_inspector(f, app, t, rest, w, value(&w.id)),
            None => {}
        }
    }
    let block = panel(t, &title, true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if app.wallets.is_empty() {
        return empty(f, inner, t, Icon::Wallet, "No wallets yet.", &[("a", "create one"), ("space i", "import a phrase")]);
    }
    let open = app.meta.as_ref().map(|m| m.id.clone());
    let wide = inner.width >= 120;
    let now = wallet_core::registry::now();
    let body = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let offset = app.list_window(app.main_list(), app.selected, app.wallets.len(), body.height as usize);
    app.hits.borrow_mut().rows(app.main_list(), body, offset, app.wallets.len(), |i| app.wallets.get(i).map(|w| w.id.clone()));
    let rows: Vec<Row> = app
        .wallets
        .iter()
        .enumerate()
        .skip(offset)
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
                .map(|s| num::qi(s.qi.parse().unwrap_or_default()))
                .map_or_else(|| Span::styled("—", t.dim_style()), |q| Span::styled(q, Style::default().fg(t.qi)));
            let worth = value(&w.id).map_or_else(|| Span::styled("—", t.dim_style()), |v| Span::styled(amount::usd(v), t.strong_style()));
            let seen = summary.map(|s| ago_short(now.saturating_sub(s.at))).unwrap_or_default();
            let mut cells = vec![
                Cell::from(Span::styled(if current { "▸" } else { " " }, Style::default().fg(t.focus))),
                Cell::from(Span::styled(truncate(&w.name, 22), t.strong_style())),
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

/// One wallet, in full: what holds its keys, what it has, and where it lives on disk.
fn draw_wallet_inspector(f: &mut Frame, app: &App, t: &Theme, area: Rect, w: &wallet_core::registry::WalletMeta, value: Option<f64>) {
    use wallet_core::registry::WalletKind;
    let open = app.meta.as_ref().is_some_and(|m| m.id == w.id);
    let title = if open { format!("wallet · {} · open", w.name) } else { format!("wallet · {}", w.name) };
    let kv = |k: &str, v: Vec<Span<'static>>| super::super::widgets::kv(t, k, v);
    let plain = |s: String| vec![Span::styled(s, t.text_style())];
    let keys = match w.kind {
        WalletKind::Hd => {
            let words = w.word_count.map(|n| format!(" · {n} words")).unwrap_or_default();
            let passphrase = if w.has_passphrase { " · with a passphrase" } else { "" };
            format!("a recovery phrase{words}{passphrase}")
        }
        WalletKind::Keys => "imported private keys".to_string(),
        WalletKind::Watch => "none here · watch-only, it cannot sign".to_string(),
    };
    let mut lines = vec![kv("keys", plain(keys))];
    if w.kind == WalletKind::Hd {
        lines.push(if w.backed_up {
            kv("backup", vec![Span::styled(format!("{}phrase verified", t.lead(Icon::Ok)), Style::default().fg(t.ok))])
        } else {
            kv("backup", vec![Span::styled(format!("{}phrase not verified", t.lead(Icon::Attention)), Style::default().fg(t.attention))])
        });
    }
    let accounts = w.quai_accounts.iter().filter(|a| !a.archived).count();
    let mut holds = Vec::new();
    if accounts > 0 || w.kind != WalletKind::Watch {
        holds.push(amount::count(accounts, "QUAI account"));
    }
    if w.payment_code.is_some() {
        holds.push("a Qi payment code".into());
    }
    if !w.watch.is_empty() {
        holds.push(format!("{} watched", amount::count(w.watch.len(), "address|addresses")));
    }
    lines.push(kv("holds", plain(holds.join(" · "))));
    let summary = app.wallet_summaries.get(&w.id);
    if let Some(v) = value {
        lines.push(kv("value", vec![Span::styled(amount::usd(v), t.strong_style())]));
    }
    if let Some(s) = summary {
        if !s.top.is_empty() {
            lines.push(kv("mostly", plain(s.top.join(" · "))));
        }
        let ago = wallet_core::registry::now().saturating_sub(s.at);
        lines.push(kv("priced", vec![Span::styled(format!("{} ago", wallet_core::track::human_duration(ago.max(60))), t.dim_style())]));
    }
    let age = wallet_core::registry::now().saturating_sub(w.created_at);
    lines.push(kv("created", vec![Span::styled(format!("{} ago", wallet_core::track::human_duration(age.max(60))), t.dim_style())]));
    let dir = app.paths.wallet_dir(&w.id).display().to_string();
    lines.push(kv("stored", vec![Span::styled(app::short_path(&dir), t.dim_style())]));
    // Long values wrap under themselves, past the label column, and the panel grows to hold them.
    let block = panel(t, &title, false);
    let frame_h = area.height.saturating_sub(block.inner(area).height);
    let width = block.inner(area).width as usize;
    let lines: Vec<Line> = lines.into_iter().flat_map(|l| super::super::widgets::hang(l, width)).collect();
    let rect = Rect { height: (lines.len() as u16 + frame_h).min(area.height), ..area };
    f.render_widget(Paragraph::new(lines).block(block), rect);
}
